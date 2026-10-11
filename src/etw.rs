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
//! Starting a kernel trace session takes Administrator rights.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, EVENT_CONTROL_CODE_ENABLE_PROVIDER,
    EVENT_HEADER_FLAG_32_BIT_HEADER, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP, EVENT_TRACE_LOGFILEW,
    EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2, OpenTraceW,
    PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME, PROCESSTRACE_HANDLE,
    ProcessTrace, StartTraceW, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};
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
/// Forget which volume each open file is on past this many, so a long run
/// doesn't grow without bound. Files opened again are learned again.
const MAX_FILES: usize = 500_000;

/// Maps a volume device such as `\Device\HarddiskVolume3` to its panel.
pub type VolumeGroups = HashMap<String, String>;

/// One thing the trace reported.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Opened { file: u64, volume: String },
    Read { pid: u32, file: u64, bytes: u64 },
    Wrote { pid: u32, file: u64, bytes: u64 },
    Received { pid: u32, bytes: u64 },
    Sent { pid: u32, bytes: u64 },
}

/// Bytes one process moved since the last sample.
#[derive(Debug, Default)]
struct Moved {
    reads: HashMap<Arc<str>, u64>,
    writes: HashMap<Arc<str>, u64>,
    received: u64,
    sent: u64,
}

/// Everything the trace has reported, shared with the callback ETW calls.
#[derive(Debug, Default)]
struct Tally {
    /// The volume each open file is on, by its file object.
    files: HashMap<u64, Arc<str>>,
    /// One shared copy of each volume name.
    volumes: HashMap<String, Arc<str>>,
    processes: HashMap<u32, Moved>,
}

impl Tally {
    fn record(&mut self, event: Event) {
        match event {
            Event::Opened { file, volume } => {
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
                self.files.insert(file, volume);
            }
            Event::Read { pid, file, bytes } | Event::Wrote { pid, file, bytes } => {
                let writing = matches!(event, Event::Wrote { .. });
                if let Some(volume) = self.files.get(&file) {
                    let moved = self.processes.entry(pid).or_default();
                    let side = if writing {
                        &mut moved.writes
                    } else {
                        &mut moved.reads
                    };
                    *side.entry(volume.clone()).or_default() += bytes;
                }
            }
            Event::Received { pid, bytes } => {
                self.processes.entry(pid).or_default().received += bytes
            }
            Event::Sent { pid, bytes } => self.processes.entry(pid).or_default().sent += bytes,
        }
    }
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
        match start_session() {
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

    /// The flows since the last call, `elapsed_secs` ago.
    pub fn sample(&mut self, elapsed_secs: f64, volumes: &VolumeGroups) -> Vec<Flow> {
        if !self.running || elapsed_secs <= 0.0 {
            return Vec::new();
        }
        let moved = match tally().lock() {
            Ok(mut tally) => std::mem::take(&mut tally.processes),
            Err(_) => return Vec::new(),
        };

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
                let process = win::process_name(pid).unwrap_or_else(|| pid.to_string());
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
                Some(Event::Opened { file, volume })
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
        tally.record(event);
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

fn start_session() -> Result<(), u32> {
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
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(on_event);
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
        tally.record(Event::Opened {
            file: 1,
            volume: r"\Device\HarddiskVolume3".into(),
        });
        tally.record(Event::Opened {
            file: 2,
            volume: r"\Device\HarddiskVolume5".into(),
        });
        tally.record(Event::Read {
            pid: 9,
            file: 1,
            bytes: 100,
        });
        tally.record(Event::Read {
            pid: 9,
            file: 1,
            bytes: 50,
        });
        tally.record(Event::Wrote {
            pid: 9,
            file: 2,
            bytes: 150,
        });
        tally.record(Event::Read {
            pid: 9,
            file: 77,
            bytes: 999,
        });
        tally.record(Event::Received { pid: 9, bytes: 10 });

        let moved = &tally.processes[&9];
        assert_eq!(moved.reads[r"\Device\HarddiskVolume3"], 150);
        assert_eq!(moved.writes[r"\Device\HarddiskVolume5"], 150);
        assert_eq!(
            moved.reads.len(),
            1,
            "a file opened before tracing is skipped"
        );
        assert_eq!(moved.received, 10);
    }
}
