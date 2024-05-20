//! Unix FS easy functions.

//! Why not use std ones? Because those expect Path, and CString is not representable as Path.

use std::ffi::CStr;
use nix::sys::stat::FileStat;
use enumn::N;

#[derive(N, Eq, PartialEq, Debug)]
#[repr(u8)]
pub enum FileType {
    // XX are these Linux-specific, use C constants?
    Pipe = 1,
    CharDevice = 2,
    Dir = 4,
    BlockDevice = 6,
    File = 8,
    Link = 10,
    Socket = 12,
}

pub trait EasyFileStat {
    fn filetype(&self) -> FileType;
}

impl EasyFileStat for FileStat {
    fn filetype(&self) -> FileType {
        FileType::n(stat_filetype(self)).expect("OS using one of the known constants")
    }
}

fn stat_filetype(st: &FileStat) -> u8 {
    ((st.st_mode & 0o0170000) >> 12) as u8
}


/// Test whether stat on `path` succeeds and yields the given file
/// type. If permissions deny access or there are disk errors, the
/// result is simply `false`.
pub fn path_is_type(path: &CStr, ftype: FileType) -> bool {
    match nix::sys::stat::stat(path) {
        Ok(m) => {
            m.filetype() == ftype
        }, 
        Err(_) => false
    }
}

pub fn path_is_file(path: &CStr) -> bool {
    path_is_type(path, FileType::File)
}
pub fn path_is_dir(path: &CStr) -> bool {
    path_is_type(path, FileType::Dir)
}
pub fn path_is_link(path: &CStr) -> bool {
    path_is_type(path, FileType::Link)
}
pub fn path_is_blockdevice(path: &CStr) -> bool {
    path_is_type(path, FileType::BlockDevice)
}
pub fn path_is_pipe(path: &CStr) -> bool {
    path_is_type(path, FileType::Pipe)
}
pub fn path_is_socket(path: &CStr) -> bool {
    path_is_type(path, FileType::Socket)
}
pub fn path_is_chardevice(path: &CStr) -> bool {
    path_is_type(path, FileType::CharDevice)
}



#[cfg(test)]
mod tests {
    use std::ffi::CString;

    use super::*;

    #[test]
    fn t_filetype() {
        fn t(f: fn(&CStr) -> bool, s: &str, expected: bool) {
            assert_eq!(f(&CString::new(s).unwrap()), expected);
        }
        t(path_is_dir, ".", true);
        t(path_is_file, ".", false);
        // Non-existing:
        t(path_is_dir, "8hbrr2kz8kmztb4dqh4", false);
        t(path_is_file, "8hbrr2kz8kmztb4dqh4", false);
        // Somewhat evil tests:
        t(path_is_file, "/etc/fstab", true);
        t(path_is_chardevice, "/dev/tty", true);
        t(path_is_chardevice, "/dev/sda", false);
        t(path_is_blockdevice, "/dev/sda", true);
    }
}
