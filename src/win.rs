//! Small Windows helpers: which kernel device a drive letter is, what a
//! process is called, and how much the network adapters have moved.

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::QueryDosDeviceW;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{HSTRING, PWSTR};

/// The kernel device behind a drive, such as `\Device\HarddiskVolume3` for
/// `C:\`. File events name files by this device rather than by letter.
pub fn volume_device(mount: &str) -> Option<String> {
    let letter = mount.get(..2).filter(|l| l.ends_with(':'))?;
    let mut buffer = [0u16; 1024];
    let written = unsafe { QueryDosDeviceW(&HSTRING::from(letter), Some(&mut buffer)) } as usize;
    // The answer is a list of strings, each ending in a nul; the first is the device.
    let first = &buffer[..written.min(buffer.len())];
    let end = first.iter().position(|&c| c == 0).unwrap_or(first.len());
    (end > 0).then(|| String::from_utf16_lossy(&first[..end]))
}

/// A process's executable name, such as `robocopy.exe`.
pub fn process_name(pid: u32) -> Option<String> {
    match pid {
        0 => return Some("Idle".to_string()),
        4 => return Some("System".to_string()),
        _ => {}
    }
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buffer = [0u16; 1024];
        let mut length = buffer.len() as u32;
        let found = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        );
        let _ = CloseHandle(process);
        found.ok()?;
        let path = String::from_utf16_lossy(&buffer[..length as usize]);
        path.rsplit('\\').next().map(str::to_string)
    }
}

/// Names and total bytes (received, sent) of the network adapters that are
/// real hardware and connected. Virtual adapters (Hyper-V, VPNs, loopback)
/// are left out, and so are the filter layers Windows stacks on each
/// adapter, which repeat its counts.
pub fn hardware_network() -> Option<(Vec<String>, (u64, u64))> {
    /// Bits of `InterfaceAndOperStatusFlags`.
    const HARDWARE: u8 = 0x1;
    const FILTER: u8 = 0x2;

    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    unsafe {
        if GetIfTable2(&mut table) != ERROR_SUCCESS || table.is_null() {
            return None;
        }
        let rows =
            std::slice::from_raw_parts((*table).Table.as_ptr(), (*table).NumEntries as usize);
        let mut names = Vec::new();
        let mut totals = (0u64, 0u64);
        for row in rows {
            let flags = row.InterfaceAndOperStatusFlags._bitfield;
            if flags & HARDWARE == 0 || flags & FILTER != 0 || row.OperStatus != IfOperStatusUp {
                continue;
            }
            let alias = &row.Alias;
            let end = alias.iter().position(|&c| c == 0).unwrap_or(alias.len());
            names.push(String::from_utf16_lossy(&alias[..end]));
            totals.0 += row.InOctets;
            totals.1 += row.OutOctets;
        }
        FreeMibTable(table as *const _);
        (!names.is_empty()).then_some((names, totals))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_drive_has_a_kernel_device() {
        let device = volume_device("C:\\").expect("C: is always mapped");
        assert!(device.starts_with(r"\Device\"), "{device}");
        assert_eq!(volume_device("not a drive"), None);
    }

    #[test]
    fn this_process_has_a_name() {
        let name = process_name(std::process::id()).unwrap();
        assert!(name.to_lowercase().ends_with(".exe"), "{name}");
        assert_eq!(process_name(4).as_deref(), Some("System"));
    }
}
