/// This is a re-implementation of `_e-gnu` from
/// https://github.com/pflanze/chj-scripts, and is internal
/// functionality called from the `e` script from the same place.

#[path = "../rawfdreader.rs"]
mod rawfdreader;
use rawfdreader::RawFdReader;
use anyhow::{Result, anyhow, bail}; 
use std::{env, writeln};
use std::io::{Write, BufReader, BufRead};
use libc::_exit;
use nix::unistd::{getpid, pipe, fork, ForkResult,
                  close, setsid, dup2, execvp, read, write};
use nix::time::{clock_gettime, ClockId};
use nix::sys::time::time_t;
use nix::fcntl::{open, OFlag};
use nix::sys::stat::{mode_t, Mode};
use nix::sys::wait::{waitpid, WaitStatus};
use std::os::unix::io::{FromRawFd, RawFd};
use std::ffi::{CString, OsString};
use std::os::unix::ffi::{OsStringExt};
use nix::sys::signal::Signal;
use bstr_parse::{BStrParse, ParseIntError};
use nix::errno::Errno;
use thiserror::Error;
use nix::unistd::Pid;


// There's no try_map, so:
fn cstrings_from_osstrings(osstrs: &mut dyn Iterator<Item = OsString>)
                           -> Result<Vec<CString>> {
    let mut v : Vec<CString> = Vec::new();
    for s in osstrs {
        v.push(CString::new(s.into_vec())?);
    }
    Ok(v)
}

fn mode_from_bits(mode: mode_t) -> Result<Mode> {
    Mode::from_bits(mode).ok_or_else(
        || anyhow!("invalid mode: {}", mode))
}

fn write_all(out: RawFd, s: &[u8]) -> Result<()> {
    let mut lentotal // : size_t
        = 0;
    let end = s.len();
    while lentotal < end {
        let len = write(out, &s[lentotal..end])?;
        lentotal += len;
    }
    Ok(())
}

fn string_matches_start(s: &str, pat: &str) -> bool {
    pat.len() <= s.len() && pat.as_bytes() == &s.as_bytes()[0..pat.len()]
}

fn string_remove_start<'ts>(s: &'ts str, pat: &str) -> &'ts str {
    if string_matches_start(s, pat) {
        &s[pat.len()..]
    } else {
        s
    }
}

fn time() -> Result<time_t> {
    Ok(clock_gettime(ClockId::CLOCK_REALTIME)?.tv_sec())
}


// waitpid_until_gone: really wait until the given process has ended,
// and return a simpler enum.

enum Status { Normalexit(i32), Signalexit(Signal) }

//fn waitpid_until_gone<P: Into<Option<Pid>>>(pid: P) -> Result<Status> {
fn waitpid_until_gone(pid: Pid) -> Result<Status> {
    loop {
        let st = waitpid(pid, None)?;
        match st {
            WaitStatus::Exited(_pid, exitcode)
                => return Ok(Status::Normalexit(exitcode)),
            WaitStatus::Signaled(_pid, signal, _bool)
                => return Ok(Status::Signalexit(signal)),
            _ => {} // retry
        }
    }
}

// Treat non-exit(0) cases as errors.
fn xwaitpid_until_gone(pid: Pid) -> Result<()> {
    match waitpid_until_gone(pid)? {
        Status::Normalexit(0) =>
            Ok(()),
        Status::Normalexit(exitcode) =>
            bail!("process exited with error code {}", exitcode),
        Status::Signalexit(signal) =>
            bail!("process exited via signal {}", signal)
    }
}

// Don't make it overly complicated, please. The original API is
// simple enough. If a Pid is given, it's the parent.
//
// Do not swallow the unsafe. Fork should be safe in our usage
// though: it should be safe even with allocation in the child as:
//  - we should not be using threading in this program (libs, though?)
//  - isn't libc's malloc safe anyway with fork?
//  - and we're not (consciously) touching any other mutexes in the children.
//
unsafe fn easy_fork() -> Result<Option<Pid>> {
    match fork()? {
        ForkResult::Parent { child, .. } => Ok(Some(child)),
        ForkResult::Child => Ok(None)
    }
}

#[derive(Error, Debug)]
enum Slurp256ParseError {
    #[error("I/O error: {0}")]
    Io(Errno),
    #[error("input is too large")]
    InputTooLarge,
    #[error("parse error: {0} for input: {1:?}")]
    Pie(ParseIntError, Vec<u8>)
}

fn slurp256_parse(fd: RawFd) -> Result<i32, Slurp256ParseError> {
    let mut buf : [u8; 257] = [0; 257];
    let len = read(fd, &mut buf).map_err(Slurp256ParseError::Io)?;
    if len == 257 {
        Err(Slurp256ParseError::InputTooLarge)
    } else {
        buf[0..len].parse().or_else(
            |e| Err(Slurp256ParseError::Pie(e, Vec::from(&buf[0..len]))))
    }
}

fn main() -> Result<()> {

    let alternate_editor = env::var_os("EMACS_ALTERNATE_EDITOR")
        .unwrap_or(OsString::from(""));
    // println!("alternate_editor={:?}", alternate_editor);

    let logpath = {
        let mut home = env::var_os("HOME").ok_or_else(
            || anyhow!("missing HOME var"))?;
        home.push("/._e-gnu_rs.log");
        home
    };

    let (sigr, sigw) = pipe()?;

    if let Some(daemonizerpid) = unsafe { easy_fork() }? {
        // println!("in parent, child={}", child);
        xwaitpid_until_gone(daemonizerpid)?;
        close(sigw)?;
        // block until buffer is closed, or more precisely, emacsclient is
        // finished, and receive its status:

        let exitcode : i32 = slurp256_parse(sigr)?;
        
        // my $statuscode= $1+0;
        // my $exitcode= $statuscode >> 8;
        // my $signal= $statuscode & 255;
        // # print "exited with exitcode=$exitcode and signal=$signal\n";
        // if ($signal) {
        //     kill $signal, $$;
        //     exit 99; # whatever, just in case we're not being terminated
        // } else {
        //     exit $exitcode;
        // }
        unsafe { _exit(exitcode) };
    } else {
        close(sigr)?;
        setsid()?; // prevent signals from crossing over (stop ctl-c)
        let (streamr, streamw) = pipe()?;
        if let Some(pid) = unsafe { easy_fork() }? {
            close(streamw)?;
            {
                // XX does RawFd have a drop that closes? Should it?
                let log : RawFd = open(
                    logpath.as_os_str(),
                    OFlag::O_CREAT |
                    OFlag::O_WRONLY |
                    OFlag::O_APPEND,
                    mode_from_bits(0o600)?)?;
                let reader = BufReader::new(
                    unsafe { RawFdReader::from_raw_fd(streamr) });
                let mut have_written = false;
                for line in reader.lines() {
                    let line = line?;
                    let line = string_remove_start(
                        &line, "Waiting for Emacs...");
                    if line.len() > 0 {
                        let mut buf = Vec::new();
                        writeln!(&mut buf, "{}\t({})\t{}",
                                 time()?, getpid(), line)?;
                        write_all(log, &buf)?;
                        if !have_written {
                            eprintln!("starting Emacs instance");
                            have_written = true;
                        }
                    }
                }
	        close(streamr)?;
                close(log)?;
            }
            
            let status = waitpid_until_gone(pid)?;
            // What's the best exit code to report a signal?
            let exitcode = if let Status::Normalexit(code) = status {
                code
            } else {
                13
            };
            let mut buf = Vec::new();
            write!(&mut buf, "{}", exitcode)?;
            // Ignore PIPE errors (in case the front process was
            // killed by user; the Perl version was simply killed by
            // SIGPIPE before it had a chance to print the error
            // message about not being able to print to sigw):
            let _ = write_all(sigw, &buf);
            let _ = close(sigw);
            unsafe { _exit(0) };
        } else {
	    close(streamr)?;
	    close(sigw)?;
	    dup2(streamw, 1)?;
	    dup2(streamw, 2)?;
            close(streamw)?;

            let cmd = {
                let mut c = OsString::from("--alternate-editor=");
                c.push(alternate_editor);
                let mut cmd = vec!(
                    CString::new("emacsclient")?,
                    CString::new("-c")?,
                    CString::new(c.into_vec())?);
                cmd.append(&mut cstrings_from_osstrings(
                    &mut env::args_os().skip(1))?);
                cmd
            };
	    execvp(&cmd[0], &cmd)?;
        }
    }

    Ok(())
}
