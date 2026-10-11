//! Real flows on Windows, from Event Tracing for Windows (ETW).
//!
//! A real-time trace session listens to two kernel providers.
//! Kernel-File reports every file opened, read and written, with the
//! process doing it; files are traced to a drive by the volume their path
//! starts with. Kernel-Network reports the TCP and UDP traffic each process
//! sends and receives. Between samples the bytes add up per process, and
//! [`attribution`] turns them into copies, downloads and uploads.
//!
//! Files on network shares are on the `\Device\Mup` volume, which maps to
//! the network panel, so copying from a share shows as a download.
//!
//! ETW hands events over in batches, some up to a second after they
//! happened. Counting them as they arrive would make a steady copy look
//! like it surges and stalls, so bytes are counted by when they actually
//! moved, and a sample only takes the time slots whose events have all
//! arrived. Flows show up about a second and a half late, at a steady rate.
//!
//! Starting a kernel trace session takes Administrator rights.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, OpenTraceW,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_RAW_TIMESTAMP,
    PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE, ProcessTrace, StartTraceW,
    TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::core::{GUID, HSTRING, PWSTR};

use crate::attribution::{self, ProcessIo};
use crate::model::Flow;
use crate::win;

/// The trace session's name. A session left behind by a disktop that
/// didn't exit cleanly is stopped before a new one starts.
const SESSION: &str = "disktop";
/// Microsoft-Windows-Kernel-File.
const FILE_PROVIDER: GUID = GUID::from_u128(0xEDD08927_9CC4_4E65_B970_C2560FB5C289);
/// Microsoft-Windows-Kernel-Network.
const NETWORK_PROVIDER: GUID = GUID::from_u128(0x7DD42A49_5329_4832_8DFD_43D979153A88);
/// Kernel-File keywords for creating, reading and writing files.
const FILE_KEYWORDS: u64 = 0x80 | 0x100 | 0x200;
/// Kernel-Network keywords for IPv4 and IPv6.
const NETWORK_KEYWORDS: u64 = 0x10 | 0x20;
/// Kernel-File event ids.
const FILE_CREATE: u16 = 12;
const FILE_READ: u16 = 15;
const FILE_WRITE: u16 = 16;
/// The I/O request flag for paging: the memory manager filling or flushing
/// the file cache, which repeats bytes a program already read or wrote.
const IRP_PAGING_IO: u32 = 0x2;
/// Where files on network shares live.
pub const NETWORK_VOLUME: &str = r"\Device\Mup";
/// The System process, which does I/O for the kernel.
const SYSTEM_PID: u32 = 4;
/// Forget which volume each open file is on past this many, so a long run
/// doesn't grow without bound. Files opened again are learned again.
const MAX_FILES: usize = 500_000;
/// Bytes are counted in time slots this many to a second.
const SLOTS_PER_SECOND: i64 = 10;
/// How many slots back a sample stops: past the session's one-second
/// flush timer, by when every event in them has arrived.
const ARRIVAL_DELAY_SLOTS: i64 = 15;
/// Slots kept while nothing samples them, such as while paused.
const MAX_SLOTS: usize = 600;
/// Forget process names past this many.
const MAX_NAMES: usize = 10_000;

/// Maps a volume device such as `\Device\HarddiskVolume3` to its panel.
pub type VolumeGroups = HashMap<String, String>;

/// One thing the trace reported.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Opened { pid: u32, file: u64, volume: String },
    Read { pid: u32, file: u64, bytes: u64 },
    Wrote { pid: u32, file: u64, bytes: u64 },
    Received { pid: u32, bytes: u64 },
    Sent { pid: u32, bytes: u64 },
}

/// Bytes one process moved.
#[derive(Debug, Default)]
struct Moved {
    /// The process's name, as it was while moving them.
    name: Option<Arc<str>>,
    reads: HashMap<Arc<str>, u64>,
    writes: HashMap<Arc<str>, u64>,
    received: u64,
    sent: u64,
}

impl Moved {
    fn add(&mut self, other: Moved) {
        for (mine, theirs) in [
            (&mut self.reads, other.reads),
            (&mut self.writes, other.writes),
        ] {
            for (volume, bytes) in theirs {
                *mine.entry(volume).or_default() += bytes;
            }
        }
        self.received += other.received;
        self.sent += other.sent;
        if other.name.is_some() {
            self.name = other.name;
        }
    }
}

/// Everything the trace has reported, shared with the callback ETW calls.
#[derive(Debug, Default)]
struct Tally {
    /// The volume each open file is on, and the process that opened it,
    /// by its file object.
    files: HashMap<u64, (Arc<str>, u32)>,
    /// One shared copy of each volume name.
    volumes: HashMap<String, Arc<str>>,
    /// What each process moved, by the time slot it moved in.
    slots: BTreeMap<i64, HashMap<u32, Moved>>,
    /// The first slot no sample has taken yet.
    open_from: i64,
    /// Each process's name and the slot it was looked up in.
    names: HashMap<u32, (Option<Arc<str>>, i64)>,
    /// How to look up a process's name; left unset in tests.
    name_of: Option<fn(u32) -> Option<String>>,
}

impl Tally {
    /// Counts an event that happened in time slot `slot`.
    fn record(&mut self, event: Event, slot: i64) {
        match event {
            Event::Opened { pid, file, volume } => {
                if self.files.len() >= MAX_FILES {
                    self.files.clear();
                }
                let volume = match self.volumes.get(&volume) {
                    Some(shared) => shared.clone(),
                    None => {
                        let shared: Arc<str> = Arc::from(volume.as_str());
                        self.volumes.insert(volume, shared.clone());
                        shared
                    }
                };
                self.files.insert(file, (volume, pid));
            }
            Event::Read { pid, file, bytes } | Event::Wrote { pid, file, bytes } => {
                let writing = matches!(event, Event::Wrote { .. });
                if let Some((volume, opener)) = self.files.get(&file).cloned() {
                    // Windows 11 copies files in the kernel, so a program's
                    // copy shows up as the System process writing a file
                    // the program opened. It counts for the program.
                    let pid = if pid == SYSTEM_PID { opener } else { pid };
                    let moved = self.moved(pid, slot);
                    let side = if writing {
                        &mut moved.writes
                    } else {
                        &mut moved.reads
                    };
                    *side.entry(volume).or_default() += bytes;
                }
            }
            Event::Received { pid, bytes } => self.moved(pid, slot).received += bytes,
            Event::Sent { pid, bytes } => self.moved(pid, slot).sent += bytes,
        }
    }

    /// Where to count what `pid` moved in `slot`. An event arriving after a
    /// sample took its slot counts toward the next sample instead.
    fn moved(&mut self, pid: u32, slot: i64) -> &mut Moved {
        if self.slots.len() >= MAX_SLOTS
            && let Some((oldest, _)) = self.slots.pop_first()
        {
            self.open_from = self.open_from.max(oldest + 1);
        }
        let slot = slot.max(self.open_from);
        let seen = self.slots.get(&slot).is_some_and(|p| p.contains_key(&pid));
        let name = if seen { None } else { self.name(pid, slot) };
        let moved = self.slots.entry(slot).or_default().entry(pid).or_default();
        if name.is_some() {
            moved.name = name;
        }
        moved
    }

    /// A process's name, looked up while it's moving data: by the time a
    /// sample comes along, a short-lived process may be gone and its id
    /// given to another. A busy process is looked up again once a second,
    /// in case its id was reused; one that has exited keeps its last name.
    fn name(&mut self, pid: u32, slot: i64) -> Option<Arc<str>> {
        if let Some((name, looked_up)) = self.names.get(&pid)
            && slot - looked_up < SLOTS_PER_SECOND
        {
            return name.clone();
        }
        let found = self
            .name_of
            .and_then(|name_of| name_of(pid))
            .map(Arc::from)
            .or_else(|| self.names.get(&pid).and_then(|(name, _)| name.clone()));
        if self.names.len() >= MAX_NAMES {
            self.names.clear();
        }
        self.names.insert(pid, (found.clone(), slot));
        found
    }

    /// Takes what each process moved before slot `until`, along with the
    /// first slot that covers.
    fn take_before(&mut self, until: i64) -> (i64, HashMap<u32, Moved>) {
        let from = self.open_from;
        let later = self.slots.split_off(&until);
        let taken = std::mem::replace(&mut self.slots, later);
        self.open_from = from.max(until);
        let mut processes: HashMap<u32, Moved> = HashMap::new();
        for (pid, moved) in taken.into_values().flatten() {
            processes.entry(pid).or_default().add(moved);
        }
        (from, processes)
    }
}

/// The time slot a performance-counter reading falls in.
fn slot_of(ticks: i64) -> i64 {
    static FREQUENCY: OnceLock<i64> = OnceLock::new();
    let frequency = *FREQUENCY.get_or_init(|| {
        let mut frequency = 0;
        let _ = unsafe { QueryPerformanceFrequency(&mut frequency) };
        frequency.max(1)
    });
    (i128::from(ticks) * i128::from(SLOTS_PER_SECOND) / i128::from(frequency)) as i64
}

/// The time slot it is now.
fn now_slot() -> i64 {
    let mut ticks = 0;
    let _ = unsafe { QueryPerformanceCounter(&mut ticks) };
    slot_of(ticks)
}

fn tally() -> &'static Mutex<Tally> {
    static TALLY: OnceLock<Mutex<Tally>> = OnceLock::new();
    TALLY.get_or_init(Mutex::default)
}

/// Flows on Windows, from a kernel trace session running in the background.
pub struct EtwFlows {
    running: bool,
    /// Why there's no trace, shown in the footer.
    problem: Option<String>,
}

impl EtwFlows {
    /// Starts the trace session. Without Administrator rights this fails,
    /// and [`EtwFlows::problem`] says so.
    pub fn start() -> Self {
        if let Ok(mut tally) = tally().lock() {
            tally.slots.clear();
            tally.open_from = now_slot();
            tally.name_of = Some(win::process_name);
        }
        match start_session(on_event) {
            Ok(()) => Self {
                running: true,
                problem: None,
            },
            Err(code) if code == ERROR_ACCESS_DENIED.0 => Self {
                running: false,
                problem: Some("flows: run as Administrator to see them".to_string()),
            },
            Err(code) => Self {
                running: false,
                problem: Some(format!("flows: tracing failed (error {code})")),
            },
        }
    }

    pub fn problem(&self) -> Option<&str> {
        self.problem.as_deref()
    }

    /// The flows from where the last call left off until a moment ago, as
    /// late as every event has surely arrived.
    pub fn sample(&mut self, volumes: &VolumeGroups) -> Vec<Flow> {
        if !self.running {
            return Vec::new();
        }
        let until = now_slot() - ARRIVAL_DELAY_SLOTS;
        let (from, moved) = match tally().lock() {
            Ok(mut tally) => tally.take_before(until),
            Err(_) => return Vec::new(),
        };
        let elapsed_secs = (until - from) as f64 / SLOTS_PER_SECOND as f64;
        if elapsed_secs <= 0.0 {
            return Vec::new();
        }

        let mut flows = Vec::new();
        for (pid, moved) in moved {
            let per_second = |bytes: u64| bytes as f64 / elapsed_secs;
            let by_group = |by_volume: &HashMap<Arc<str>, u64>| {
                let mut groups: Vec<(String, f64)> = Vec::new();
                for (volume, &bytes) in by_volume {
                    let Some(group) = volumes.get(&**volume) else {
                        continue;
                    };
                    match groups.iter_mut().find(|(g, _)| g == group) {
                        Some((_, rate)) => *rate += per_second(bytes),
                        None => groups.push((group.clone(), per_second(bytes))),
                    }
                }
                groups
            };
            let io = ProcessIo {
                reads: by_group(&moved.reads),
                writes: by_group(&moved.writes),
                received: per_second(moved.received),
                sent: per_second(moved.sent),
            };
            if io.is_busy() {
                let process = moved
                    .name
                    .as_deref()
                    .map_or_else(|| pid.to_string(), str::to_string);
                attribution::add_process_flows(&mut flows, &process, &io);
            }
        }
        attribution::finish(flows)
    }
}

impl Drop for EtwFlows {
    fn drop(&mut self) {
        if self.running {
            stop_session();
        }
    }
}

/// Reads one event's fields from its raw data. `pointer` is the size of a
/// pointer in the event: 8, or 4 when it came from a 32-bit system. The
/// layouts are checked against Windows' own definitions by
/// `layouts_match_windows`.
fn parse(
    provider: GUID,
    id: u16,
    version: u8,
    pointer: usize,
    pid: u32,
    data: &[u8],
) -> Option<Event> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
    };
    let pointer_at = |at: usize| -> Option<u64> {
        if pointer == 8 {
            Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
        } else {
            u32_at(at).map(u64::from)
        }
    };

    if provider == FILE_PROVIDER {
        match id {
            FILE_CREATE => {
                // Irp, [ThreadId in version 0], FileObject, then four 32-bit
                // fields in version 1 (three in version 0), then the name.
                let (file_at, name_at) = if version == 0 {
                    (2 * pointer, 3 * pointer + 12)
                } else {
                    (pointer, 2 * pointer + 16)
                };
                let file = pointer_at(file_at)?;
                let volume = volume_of(&utf16_until_nul(data.get(name_at..)?))?;
                Some(Event::Opened { pid, file, volume })
            }
            FILE_READ | FILE_WRITE => {
                // ByteOffset, Irp, [ThreadId in version 0], FileObject,
                // FileKey, [ThreadId in version 1], IOSize, IOFlags.
                let (file_at, size_at) = if version == 0 {
                    (8 + 2 * pointer, 8 + 4 * pointer)
                } else {
                    (8 + pointer, 8 + 3 * pointer + 4)
                };
                if u32_at(size_at + 4)? & IRP_PAGING_IO != 0 {
                    return None;
                }
                let file = pointer_at(file_at)?;
                let bytes = u64::from(u32_at(size_at)?);
                Some(if id == FILE_READ {
                    Event::Read { pid, file, bytes }
                } else {
                    Event::Wrote { pid, file, bytes }
                })
            }
            _ => None,
        }
    } else if provider == NETWORK_PROVIDER {
        // TCP and UDP, IPv4 and IPv6: PID, size, destination, source.
        let (sending, address_len) = match id {
            10 | 42 => (true, 4),
            11 | 43 => (false, 4),
            26 | 58 => (true, 16),
            27 | 59 => (false, 16),
            _ => return None,
        };
        let pid = u32_at(0)?;
        let bytes = u64::from(u32_at(4)?);
        let destination = data.get(8..8 + address_len)?;
        let source = data.get(8 + address_len..8 + 2 * address_len)?;
        if is_loopback(destination) || is_loopback(source) {
            return None;
        }
        Some(if sending {
            Event::Sent { pid, bytes }
        } else {
            Event::Received { pid, bytes }
        })
    } else {
        None
    }
}

/// The volume a kernel file path is on: `\Device\HarddiskVolume3` for
/// `\Device\HarddiskVolume3\Users\me\file.txt`.
fn volume_of(path: &str) -> Option<String> {
    let mut parts = path.splitn(4, '\\');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(""), Some("Device"), Some(volume)) if !volume.is_empty() => {
            Some(format!(r"\Device\{volume}"))
        }
        _ => None,
    }
}

/// A nul-terminated UTF-16 string at the start of `bytes`.
fn utf16_until_nul(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&pair| u16::from_le_bytes(pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Whether an IPv4 or IPv6 address, in network order, is this machine:
/// 127.x.x.x, `::1`, or `::ffff:127.x.x.x`.
fn is_loopback(address: &[u8]) -> bool {
    match address.len() {
        4 => address[0] == 127,
        16 => {
            let zeros = |range: std::ops::Range<usize>| address[range].iter().all(|&b| b == 0);
            (zeros(0..15) && address[15] == 1)
                || (zeros(0..10) && address[10..12] == [0xff, 0xff] && address[12] == 127)
        }
        _ => false,
    }
}

/// Called by ETW on its own thread for every event.
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    let Some(record) = (unsafe { record.as_ref() }) else {
        return;
    };
    let header = &record.EventHeader;
    let data: &[u8] = if record.UserData.is_null() {
        &[]
    } else {
        unsafe {
            std::slice::from_raw_parts(
                record.UserData as *const u8,
                usize::from(record.UserDataLength),
            )
        }
    };
    let pointer = if u32::from(header.Flags) & EVENT_HEADER_FLAG_32_BIT_HEADER != 0 {
        4
    } else {
        8
    };
    let descriptor = &header.EventDescriptor;
    if let Some(event) = parse(
        header.ProviderId,
        descriptor.Id,
        descriptor.Version,
        pointer,
        header.ProcessId,
        data,
    ) && let Ok(mut tally) = tally().lock()
    {
        // A raw performance-counter reading, the same clock as now_slot.
        tally.record(event, slot_of(header.TimeStamp));
    }
}

/// `EVENT_TRACE_PROPERTIES` followed by room for the session name, as
/// `StartTraceW` and `ControlTraceW` want it. Kept in `u64`s so the struct
/// is properly aligned.
fn session_properties() -> Vec<u64> {
    let header = size_of::<EVENT_TRACE_PROPERTIES>();
    let total = header + (SESSION.len() + 1) * 2;
    let mut buffer = vec![0u64; total.div_ceil(8)];
    let properties = buffer.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES;
    unsafe {
        (*properties).Wnode.BufferSize = total as u32;
        (*properties).Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        // Timestamps from the performance counter.
        (*properties).Wnode.ClientContext = 1;
        (*properties).LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        (*properties).LoggerNameOffset = header as u32;
        // Small buffers flushed every second, so events arrive promptly.
        (*properties).BufferSize = 16;
        (*properties).FlushTimer = 1;
    }
    buffer
}

/// Starts the session, with `callback` called for every event.
fn start_session(callback: unsafe extern "system" fn(*mut EVENT_RECORD)) -> Result<(), u32> {
    stop_session();
    let mut properties = session_properties();
    let mut session = CONTROLTRACE_HANDLE::default();
    unsafe {
        let started = StartTraceW(
            &mut session,
            &HSTRING::from(SESSION),
            properties.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
        );
        if started != ERROR_SUCCESS {
            return Err(started.0);
        }
        for (provider, keywords) in [
            (FILE_PROVIDER, FILE_KEYWORDS),
            (NETWORK_PROVIDER, NETWORK_KEYWORDS),
        ] {
            let enabled = EnableTraceEx2(
                session,
                &provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                TRACE_LEVEL_INFORMATION as u8,
                keywords,
                0,
                0,
                None,
            );
            if enabled != ERROR_SUCCESS {
                stop_session();
                return Err(enabled.0);
            }
        }

        let mut name: Vec<u16> = SESSION.encode_utf16().chain([0]).collect();
        let mut logfile = EVENT_TRACE_LOGFILEW {
            LoggerName: PWSTR(name.as_mut_ptr()),
            ..Default::default()
        };
        // Raw timestamps leave event times as performance-counter readings.
        logfile.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME
            | PROCESS_TRACE_MODE_EVENT_RECORD
            | PROCESS_TRACE_MODE_RAW_TIMESTAMP;
        logfile.Anonymous2.EventRecordCallback = Some(callback);
        let consumer = OpenTraceW(&mut logfile);
        if consumer.Value == u64::MAX {
            stop_session();
            return Err(u32::MAX);
        }
        let consumer = consumer.Value;
        // ProcessTrace delivers events until the session stops.
        thread::Builder::new()
            .name("etw".to_string())
            .spawn(move || {
                let handle = PROCESSTRACE_HANDLE { Value: consumer };
                let _ = ProcessTrace(&[handle], None, None);
            })
            .map_err(|_| u32::MAX)?;
    }
    Ok(())
}

fn stop_session() {
    let mut properties = session_properties();
    unsafe {
        let _ = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            &HSTRING::from(SESSION),
            properties.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES,
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Diagnostics::Etw::{
        EVENT_DESCRIPTOR, TRACE_EVENT_INFO, TdhGetManifestEventInformation,
    };

    use super::*;

    /// Field names and types of an event, from Windows' own definition.
    fn fields(provider: GUID, id: u16, version: u8) -> Vec<(String, u16)> {
        let descriptor = EVENT_DESCRIPTOR {
            Id: id,
            Version: version,
            ..Default::default()
        };
        let mut size = 0u32;
        unsafe {
            TdhGetManifestEventInformation(&provider, &descriptor, None, &mut size);
            let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
            let info = buffer.as_mut_ptr() as *mut TRACE_EVENT_INFO;
            assert_eq!(
                TdhGetManifestEventInformation(&provider, &descriptor, Some(info), &mut size),
                0,
                "event {id} version {version}"
            );
            let count = (*info).TopLevelPropertyCount as usize;
            let properties =
                std::slice::from_raw_parts((*info).EventPropertyInfoArray.as_ptr(), count);
            let base = buffer.as_ptr() as *const u8;
            properties
                .iter()
                .map(|p| {
                    let name = base.add(p.NameOffset as usize) as *const u16;
                    let len = (0..).take_while(|&i| *name.add(i) != 0).count();
                    let name = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
                    (name, p.Anonymous1.nonStructType.InType)
                })
                .collect()
        }
    }

    /// The names of the fields up to and including `last`, in order, with
    /// pointers (type 16), 32-bit numbers (8), 64-bit numbers (10), 16-bit
    /// numbers (6), binary (14) and strings (1).
    fn names(fields: &[(String, u16)], last: &str) -> Vec<String> {
        let end = fields.iter().position(|(n, _)| n == last).unwrap() + 1;
        fields[..end]
            .iter()
            .map(|(n, t)| format!("{n}:{t}"))
            .collect()
    }

    #[test]
    fn layouts_match_windows() {
        let create_v0 = fields(FILE_PROVIDER, FILE_CREATE, 0);
        assert_eq!(
            names(&create_v0, "FileName"),
            [
                "Irp:16",
                "ThreadId:16",
                "FileObject:16",
                "CreateOptions:8",
                "CreateAttributes:8",
                "ShareAccess:8",
                "FileName:1"
            ]
        );
        let create_v1 = fields(FILE_PROVIDER, FILE_CREATE, 1);
        assert_eq!(
            names(&create_v1, "FileName"),
            [
                "Irp:16",
                "FileObject:16",
                "IssuingThreadId:8",
                "CreateOptions:8",
                "CreateAttributes:8",
                "ShareAccess:8",
                "FileName:1"
            ]
        );
        for id in [FILE_READ, FILE_WRITE] {
            assert_eq!(
                names(&fields(FILE_PROVIDER, id, 0), "IOFlags"),
                [
                    "ByteOffset:10",
                    "Irp:16",
                    "ThreadId:16",
                    "FileObject:16",
                    "FileKey:16",
                    "IOSize:8",
                    "IOFlags:8"
                ]
            );
            assert_eq!(
                names(&fields(FILE_PROVIDER, id, 1), "IOFlags"),
                [
                    "ByteOffset:10",
                    "Irp:16",
                    "FileObject:16",
                    "FileKey:16",
                    "IssuingThreadId:8",
                    "IOSize:8",
                    "IOFlags:8"
                ]
            );
        }
        for id in [10, 11, 42, 43] {
            assert_eq!(
                names(&fields(NETWORK_PROVIDER, id, 0), "saddr"),
                ["PID:8", "size:8", "daddr:8", "saddr:8"]
            );
        }
        for id in [26, 27, 58, 59] {
            assert_eq!(
                names(&fields(NETWORK_PROVIDER, id, 0), "saddr"),
                ["PID:8", "size:8", "daddr:14", "saddr:14"]
            );
        }
    }

    /// Builds an event's raw data from little-endian pieces.
    fn data(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    fn utf16(text: &str) -> Vec<u8> {
        text.encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect()
    }

    const FILE_OBJECT: u64 = 0xFFFF_A001_2345_6780;

    #[test]
    fn create_events_say_which_volume_a_file_is_on() {
        let name = utf16(r"\Device\HarddiskVolume3\Users\me\movie.mkv");
        let v1 = data(&[
            &7u64.to_le_bytes(),
            &FILE_OBJECT.to_le_bytes(),
            &[0; 16],
            &name,
        ]);
        let v0 = data(&[
            &7u64.to_le_bytes(),
            &9u64.to_le_bytes(),
            &FILE_OBJECT.to_le_bytes(),
            &[0; 12],
            &name,
        ]);
        let expected = Some(Event::Opened {
            pid: 0,
            file: FILE_OBJECT,
            volume: r"\Device\HarddiskVolume3".into(),
        });
        assert_eq!(parse(FILE_PROVIDER, FILE_CREATE, 1, 8, 0, &v1), expected);
        assert_eq!(parse(FILE_PROVIDER, FILE_CREATE, 0, 8, 0, &v0), expected);
    }

    #[test]
    fn reads_and_writes_carry_size_and_process() {
        let v1 = |flags: u32| {
            data(&[
                &0u64.to_le_bytes(),
                &1u64.to_le_bytes(),
                &FILE_OBJECT.to_le_bytes(),
                &2u64.to_le_bytes(),
                &3u32.to_le_bytes(),
                &65536u32.to_le_bytes(),
                &flags.to_le_bytes(),
                &0u32.to_le_bytes(),
            ])
        };
        assert_eq!(
            parse(FILE_PROVIDER, FILE_READ, 1, 8, 42, &v1(0)),
            Some(Event::Read {
                pid: 42,
                file: FILE_OBJECT,
                bytes: 65536
            })
        );
        assert_eq!(
            parse(FILE_PROVIDER, FILE_WRITE, 1, 8, 42, &v1(0x60900)),
            Some(Event::Wrote {
                pid: 42,
                file: FILE_OBJECT,
                bytes: 65536
            })
        );
        // The cache manager filling or flushing pages repeats a program's
        // own reads and writes, so it isn't counted.
        assert_eq!(
            parse(FILE_PROVIDER, FILE_WRITE, 1, 8, 4, &v1(IRP_PAGING_IO)),
            None
        );

        let v0 = data(&[
            &0u64.to_le_bytes(),
            &1u64.to_le_bytes(),
            &9u64.to_le_bytes(),
            &FILE_OBJECT.to_le_bytes(),
            &2u64.to_le_bytes(),
            &4096u32.to_le_bytes(),
            &0u32.to_le_bytes(),
        ]);
        assert_eq!(
            parse(FILE_PROVIDER, FILE_READ, 0, 8, 7, &v0),
            Some(Event::Read {
                pid: 7,
                file: FILE_OBJECT,
                bytes: 4096
            })
        );
    }

    #[test]
    fn network_events_skip_loopback() {
        let v4 = |id, to: [u8; 4], from: [u8; 4]| {
            let event = data(&[
                &1234u32.to_le_bytes(),
                &1500u32.to_le_bytes(),
                &to,
                &from,
                &[0; 8],
            ]);
            parse(NETWORK_PROVIDER, id, 0, 8, 0, &event)
        };
        assert_eq!(
            v4(10, [93, 184, 216, 34], [10, 0, 0, 5]),
            Some(Event::Sent {
                pid: 1234,
                bytes: 1500
            })
        );
        assert_eq!(
            v4(11, [10, 0, 0, 5], [93, 184, 216, 34]),
            Some(Event::Received {
                pid: 1234,
                bytes: 1500
            })
        );
        assert_eq!(v4(11, [127, 0, 0, 1], [127, 0, 0, 1]), None);

        let mut local = [0u8; 16];
        local[15] = 1;
        let mut remote = [0u8; 16];
        remote[..2].copy_from_slice(&[0x20, 0x01]);
        let v6 = |to: [u8; 16]| {
            let event = data(&[&1u32.to_le_bytes(), &9000u32.to_le_bytes(), &to, &remote]);
            parse(NETWORK_PROVIDER, 27, 0, 8, 0, &event)
        };
        assert_eq!(v6(local), None);
        assert_eq!(
            v6(remote),
            Some(Event::Received {
                pid: 1,
                bytes: 9000
            })
        );
    }

    #[test]
    fn volumes_come_from_the_start_of_kernel_paths() {
        assert_eq!(
            volume_of(r"\Device\HarddiskVolume3\Users\me").as_deref(),
            Some(r"\Device\HarddiskVolume3")
        );
        assert_eq!(
            volume_of(r"\Device\Mup\nas\share\f").as_deref(),
            Some(NETWORK_VOLUME)
        );
        assert_eq!(volume_of(r"C:\Users"), None);
        assert_eq!(volume_of(""), None);
    }

    #[test]
    fn the_tally_adds_up_bytes_per_process_and_volume() {
        let mut tally = Tally::default();
        tally.record(
            Event::Opened {
                pid: 9,
                file: 1,
                volume: r"\Device\HarddiskVolume3".into(),
            },
            0,
        );
        tally.record(
            Event::Opened {
                pid: 9,
                file: 2,
                volume: r"\Device\HarddiskVolume5".into(),
            },
            0,
        );
        let read = |bytes| Event::Read {
            pid: 9,
            file: 1,
            bytes,
        };
        tally.record(read(100), 1);
        tally.record(read(50), 2);
        tally.record(
            Event::Wrote {
                pid: 9,
                file: 2,
                bytes: 150,
            },
            2,
        );
        tally.record(
            Event::Read {
                pid: 9,
                file: 77,
                bytes: 999,
            },
            2,
        );
        tally.record(Event::Received { pid: 9, bytes: 10 }, 2);

        let (from, processes) = tally.take_before(3);
        assert_eq!(from, 0);
        let moved = &processes[&9];
        assert_eq!(moved.reads[r"\Device\HarddiskVolume3"], 150);
        assert_eq!(moved.writes[r"\Device\HarddiskVolume5"], 150);
        assert_eq!(
            moved.reads.len(),
            1,
            "a file opened before tracing is skipped"
        );
        assert_eq!(moved.received, 10);
    }

    #[test]
    fn bytes_count_toward_when_they_moved_not_when_they_arrived() {
        let mut tally = Tally {
            open_from: 10,
            ..Tally::default()
        };
        let sent = |bytes| Event::Sent { pid: 1, bytes };
        // Events from slots 10 to 14 arrive out of order.
        tally.record(sent(400), 14);
        tally.record(sent(100), 10);
        tally.record(sent(200), 12);

        // A sample up to slot 13 gets slots 10 to 12 and leaves slot 14.
        let (from, taken) = tally.take_before(13);
        assert_eq!(from, 10);
        assert_eq!(taken[&1].sent, 300);

        // Something from slot 11 arriving even later counts in the next
        // sample rather than being lost.
        tally.record(sent(50), 11);
        let (from, taken) = tally.take_before(15);
        assert_eq!(from, 13);
        assert_eq!(taken[&1].sent, 450);
    }

    /// One raw file event, for `print_events`.
    #[derive(Debug, Clone)]
    struct RawEvent {
        pid: u32,
        id: u16,
        file: u64,
        bytes: u64,
        flags: u32,
        name: String,
    }

    fn raw_events() -> &'static Mutex<Vec<RawEvent>> {
        static EVENTS: OnceLock<Mutex<Vec<RawEvent>>> = OnceLock::new();
        EVENTS.get_or_init(Mutex::default)
    }

    unsafe extern "system" fn collect_event(record: *mut EVENT_RECORD) {
        let Some(record) = (unsafe { record.as_ref() }) else {
            return;
        };
        let header = &record.EventHeader;
        if header.ProviderId != FILE_PROVIDER || header.EventDescriptor.Version != 1 {
            return;
        }
        let data = unsafe {
            std::slice::from_raw_parts(
                record.UserData as *const u8,
                usize::from(record.UserDataLength),
            )
        };
        let u32_at = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
        let u64_at = |at: usize| u64::from_le_bytes(data[at..at + 8].try_into().unwrap());
        let id = header.EventDescriptor.Id;
        let event = match id {
            FILE_CREATE => RawEvent {
                pid: header.ProcessId,
                id,
                file: u64_at(8),
                bytes: 0,
                flags: 0,
                name: utf16_until_nul(&data[32..]),
            },
            FILE_READ | FILE_WRITE => RawEvent {
                pid: header.ProcessId,
                id,
                file: u64_at(16),
                bytes: u64::from(u32_at(36)),
                flags: u32_at(40),
                name: String::new(),
            },
            _ => return,
        };
        raw_events().lock().unwrap().push(event);
    }

    /// Prints what the kernel reports about the file I/O of one command,
    /// for working out how a program's copies look to ETW. Run as
    /// Administrator with `CMD="copy a b" cargo test print_events --
    /// --ignored --nocapture`.
    #[test]
    #[ignore]
    fn print_events() {
        let command = std::env::var("CMD").expect("CMD to run");
        start_session(collect_event).expect("run as Administrator");
        thread::sleep(std::time::Duration::from_secs(2));
        use std::os::windows::process::CommandExt;

        // Passed as written, since cmd doesn't follow the usual quoting.
        let mut child = std::process::Command::new("cmd")
            .raw_arg(format!("/C {command}"))
            .spawn()
            .unwrap();
        let pid = child.id();
        child.wait().unwrap();
        thread::sleep(std::time::Duration::from_secs(3));
        stop_session();

        let events = raw_events().lock().unwrap().clone();
        let names: HashMap<u64, String> = events
            .iter()
            .filter(|e| e.id == FILE_CREATE)
            .map(|e| (e.file, e.name.clone()))
            .collect();
        println!("command ran as pid {pid}");
        // Totals by process, event, paging and whether the file is known.
        let mut totals: BTreeMap<(u32, u16, bool, String), (u64, u64)> = BTreeMap::new();
        for e in events.iter().filter(|e| e.id != FILE_CREATE) {
            let volume = names
                .get(&e.file)
                .and_then(|n| volume_of(n))
                .unwrap_or_else(|| "(opened before tracing)".into());
            let key = (e.pid, e.id, e.flags & IRP_PAGING_IO != 0, volume);
            let total = totals.entry(key).or_default();
            total.0 += 1;
            total.1 += e.bytes;
        }
        let mut rows: Vec<_> = totals.into_iter().collect();
        rows.sort_by_key(|(_, (_, bytes))| std::cmp::Reverse(*bytes));
        for ((p, id, paging, volume), (count, bytes)) in rows.into_iter().take(25) {
            let what = if id == FILE_READ { "read " } else { "write" };
            let paging = if paging { "paging" } else { "      " };
            println!("pid {p:>6} {what} {paging} {volume:<28} {count:>7} events {bytes:>12} bytes");
        }
        for e in events
            .iter()
            .filter(|e| e.pid == pid && e.id == FILE_CREATE)
        {
            println!("pid {pid} opened {}", e.name);
        }
        let mut flags: BTreeMap<u32, u64> = BTreeMap::new();
        for e in events
            .iter()
            .filter(|e| e.pid == pid && e.id != FILE_CREATE)
        {
            *flags.entry(e.flags).or_default() += 1;
        }
        println!("IOFlags of the command's reads and writes: {flags:x?}");
    }

    #[test]
    fn kernel_copies_count_for_the_program_that_opened_the_files() {
        let mut tally = Tally::default();
        let open = |file, volume: &str| Event::Opened {
            pid: 300,
            file,
            volume: volume.into(),
        };
        tally.record(open(1, r"\Device\HarddiskVolume3"), 0);
        tally.record(open(2, r"\Device\HarddiskVolume6"), 0);
        // What `copy` looks like: the program reads, the kernel writes.
        tally.record(
            Event::Read {
                pid: 300,
                file: 1,
                bytes: 100,
            },
            1,
        );
        tally.record(
            Event::Wrote {
                pid: SYSTEM_PID,
                file: 2,
                bytes: 100,
            },
            1,
        );
        let (_, taken) = tally.take_before(2);
        assert_eq!(taken.len(), 1, "nothing is left for System");
        assert_eq!(taken[&300].reads[r"\Device\HarddiskVolume3"], 100);
        assert_eq!(taken[&300].writes[r"\Device\HarddiskVolume6"], 100);
    }

    #[test]
    fn processes_are_named_while_they_move_data() {
        let mut tally = Tally {
            name_of: Some(|pid| Some(format!("app{pid}.exe"))),
            ..Tally::default()
        };
        tally.record(Event::Received { pid: 5, bytes: 1 }, 0);
        // The process exits, so looking it up again finds nothing.
        tally.name_of = Some(|_| None);
        tally.record(Event::Received { pid: 5, bytes: 1 }, 20);
        let (_, taken) = tally.take_before(30);
        assert_eq!(taken[&5].name.as_deref(), Some("app5.exe"));

        // Its id goes to another program, which is looked up anew.
        tally.name_of = Some(|_| Some("other.exe".to_string()));
        tally.record(Event::Received { pid: 5, bytes: 1 }, 40);
        let (_, taken) = tally.take_before(50);
        assert_eq!(taken[&5].name.as_deref(), Some("other.exe"));
    }

    #[test]
    fn slots_nobody_takes_are_forgotten_oldest_first() {
        let mut tally = Tally::default();
        for slot in 0..MAX_SLOTS as i64 + 10 {
            tally.record(Event::Received { pid: 1, bytes: 1 }, slot);
        }
        assert_eq!(tally.slots.len(), MAX_SLOTS);
        // A sample afterwards covers only the slots still kept.
        let (from, taken) = tally.take_before(i64::MAX);
        assert_eq!(from, 10);
        assert_eq!(taken[&1].received, MAX_SLOTS as u64);
    }
}
