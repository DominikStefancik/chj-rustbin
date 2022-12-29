/// This is a re-implementation of `_e-gnu` from
/// https://github.com/pflanze/chj-scripts, and is internal
/// functionality called from the `e` script from the same place.

#[path = "../rawfdreader.rs"]
mod rawfdreader;
use rawfdreader::RawFdReader;
use anyhow::{Result, anyhow, bail}; 
use std::{env, writeln};
use std::io::{stdin, Write, BufReader, BufRead};
use libc::_exit;
use nix::unistd::{getpid, pipe, fork, ForkResult,
                  close, setsid, dup2, execvp, read, write};
use nix::time::{clock_gettime, ClockId};
use nix::sys::time::time_t;
use nix::fcntl::{open, OFlag};
use nix::sys::stat::{mode_t, Mode};
use nix::sys::wait::{waitpid, WaitStatus};
use std::os::unix::io::{FromRawFd, RawFd};
use std::ffi::{CString, OsString, OsStr};
use std::os::unix::ffi::{OsStringExt};
use nix::sys::signal::Signal;
use bstr_parse::{BStrParse, ParseIntError, FromBStr};
use nix::errno::Errno;
use thiserror::Error;
use nix::unistd::Pid;
//use nix::sys::wait::Id::Pid;
use std::process::exit;


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

// XX replace with vfork/exec or rather posix_spawnp
unsafe fn fork_cmd(cmd: &[CString]) -> Result<Pid> {
    if let Some(pid) = easy_fork()? {
        Ok(pid)
    } else {
        execvp(&cmd[0], &cmd)?;
        Ok(Pid::from_raw(0)) // never reached, satisfy type system
    }
}


fn xcheck_exit_success(res: Result<i32>, cmd: &[CString]) -> Result<()> {
    let exitcode = res?;
    if exitcode == 0 {
        Ok(())
    } else {
        bail!("command ended with error exit code {}: {:?}",
              exitcode, cmd)
    }
}

fn ask_yn(question: &str) -> Result<bool> {
    for n in (1..5).rev() {
        println!("{} (y/n)", question);
        let mut ans = String::new();
        stdin().read_line(&mut ans)?;
        if ans.len() > 1 && ans.starts_with("y") {
            return Ok(true)
        } else if ans.len() > 1 && ans.starts_with("n") {
            return Ok(false)
        }
        println!("please answer with y or n, {} tries left", n);
    }
    bail!("could not get an answer to the question {:?}",
          question)
}


#[derive(Error, Debug)]
enum Slurp256Error {
    #[error("I/O error: {0}")]
    Io(Errno),
    #[error("input is too large")]
    InputTooLarge,
    #[error("parse error: {0} for input: {1:?}")]
    NoParse(ParseIntError, Vec<u8>),
}

fn slurp256_parse<T: FromBStr<Err = bstr_parse::ParseIntError>>(
    fd: RawFd,
    do_chomp: bool,
) -> Result<T, Slurp256Error> {
    let mut buf : [u8; 257] = [0; 257];
    let len = read(fd, &mut buf).map_err(Slurp256Error::Io)?;
    close(fd).or_else(|e| Err(Slurp256Error::Io(e)))?;
    if len == 257 {
        return Err(Slurp256Error::InputTooLarge)
    }
    let end =
        if do_chomp && len > 0 {
            (|| {
                for i in (0..len-1).rev() {
                    if buf[i] != b'\n' {
                        return i+1
                    }
                }
                0
            })()
        } else {
            len
        };
    let s = &buf[0..end];
    s.parse().or_else(
        |e| Err(Slurp256Error::NoParse(e, Vec::from(s))))
}


fn backtick<T: 'static + Send + Sync + std::fmt::Debug + std::fmt::Display
            + FromBStr<Err = bstr_parse::ParseIntError>>(
    cmd: &Vec<CString>,
    do_chomp: bool,
) -> Result<T> {
    let (streamr, streamw) = pipe()?;
    if let Some(pid) = unsafe { easy_fork() }? {
        close(streamw)?;
        let x = slurp256_parse(streamr, do_chomp)?;
        xwaitpid_until_gone(pid)?;
        Ok(x)
    } else {
        close(streamr)?;
        dup2(streamw, 1)?;
        // dup2(streamw, 2)?;
        close(streamw)?;

        execvp(&cmd[0], &cmd)?;
        unsafe { _exit(123) }; // never reached, to satisfy type system
    }
}

// Run cmd, waiting for its exit and logging its output.
fn run_cmd_with_log(cmd: &Vec<CString>, logpath: &OsStr) -> Result<i32> {
    let (streamr, streamw) = pipe()?;
    if let Some(pid) = unsafe { easy_fork() }? {
        close(streamw)?;
        {
            // XX does RawFd have a drop that closes? Should it?
            let log : RawFd = open(
                logpath,
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
                    // emacsclient *always* prints this (to
                    // indicate that the buffer needs to be
                    // closed)
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
        let exitcode =
            if let Status::Normalexit(code) = status {
                code
            } else {
                13
            };
        Ok(exitcode)
    } else {
        close(streamr)?;
        // close(sigw)?; -- XX should close that, where?
        dup2(streamw, 1)?;
        dup2(streamw, 2)?;
        close(streamw)?;

        execvp(&cmd[0], &cmd)?;
        Ok(0) // in child, never reached, just to satisfy type system
    }
}


fn main() -> Result<()> {

    let (args, args_is_all_files) = (|| -> Result<(Vec<CString>, bool)> {
        let args = cstrings_from_osstrings(&mut env::args_os().skip(1))?;
        let mut files : Vec<CString> = Vec::new();
        let mut seen_boundary = false; // seen "--"
        for arg in &args {
            let a = arg.to_bytes();
            if a == b"--" {
                seen_boundary = true;
            } else if ! seen_boundary && a.starts_with(b"-") {
                println!("can't currently deal with options, falling \
                          back to single emacsclient call (not opening \
                          a separate frame per file)");
                return Ok((args, false))
            } else if a.starts_with(b"+") {
                println!("can't currently deal with positions, falling \
                          back to single emacsclient call (not opening \
                          a separate frame per file)");
                return Ok((args, false))
            } else {
                files.push(arg.clone());
            }
        }
        Ok((files, true))
    })()?;
    
    let logpath = {
        let mut home = env::var_os("HOME").ok_or_else(
            || anyhow!("missing HOME var"))?;
        home.push("/._e-gnu_rs.log");
        home
    };

    let daemonwork : Box<dyn FnOnce(&OsStr) -> Result<i32>> =
        if !args_is_all_files || args.len() == 1 {
            // Let emacsclient start the daemon on its own if
            // necessary. That way we need to run just one command.

            // XX What do we do with this env var?:
            // let alternate_editor = env::var_os("ALTERNATE_EDITOR")
            //     .unwrap_or(OsString::from(""));
            // println!("alternate_editor={:?}", alternate_editor);

            let mut cmd = vec!(
                CString::new("emacsclient")?,
                CString::new("-c")?,
                {
                    let alt = OsString::from("--alternate-editor=");
                    // alt.push(alternate_editor);
                    CString::new(alt.into_vec())?
                }
            );
            cmd.append(&mut args.clone());

            Box::new(move |logpath| run_cmd_with_log(&cmd, logpath))

        } else {
            let files = args;
            if files.len() > 8 {
                if ! ask_yn(&format!("got {} arguments, do you really want to open so many windows?",
                                     files.len()))? {
                    println!("cancelling");
                    return Ok(());
                }
            }

            Box::new(|logpath| {
                // Check if emacs daemon is up, if not, start it. Then
                // open each file (args is just files here) with a
                // separate emacsclient call, so that each is opened in a
                // separate frame.

                let start_emacs = || -> Result<()> {
                    let cmd = vec!(CString::new("emacs")?,
                                   CString::new("--daemon")?);
                    xcheck_exit_success(
                        run_cmd_with_log(&cmd,
                                         logpath),
                        &cmd)?;
                    Ok(())
                };

                let res : Result<i32> = backtick(
                    &vec!(CString::new("emacsclient")?,
                          CString::new("-e")?,
                          CString::new("(+ 3 2)")?),
                    true);
                // println!("res= {:?}", res);
                match res {
                    Err(_) => {
                        start_emacs()?
                    },
                    Ok(val) => {
                        if val == 5 {
                            // Emacs is already up
                        } else {
                            start_emacs()?
                        }
                    }
                }

                // Open each file separately, collecting the pids that
                // we then wait on.
                let mut pids = Vec::new();
                for file in files {
                    let cmd = vec!(
                        CString::new("emacsclient")?,
                        CString::new("-c")?,
                        file);
                    let pid = unsafe { fork_cmd(&cmd) }?;
                    pids.push(pid);
                }
                // Collecting them out of their exit order. Only
                // matters for early termination in case of errors
                // (and to avoid zombies). Does anyone care?
                for pid in pids {
                    xwaitpid_until_gone(pid)?;
                }
                Ok(0)
            })
        };


    // Run the emacs interfacing code in a child that's protected from
    // ctl-c ("daemon").

    let (sigr, sigw) = pipe()?;

    if let Some(daemonizerpid) = unsafe { easy_fork() }? {
        // println!("in parent, child={}", child);
        xwaitpid_until_gone(daemonizerpid)?;
        close(sigw)?;
        // block until buffer is closed, or more precisely, emacsclient is
        // finished, and receive its status:

        let exitcode : i32 = slurp256_parse(sigr, false)?;
        
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
        exit(exitcode);
    } else {
        close(sigr)?;
        setsid()?; // prevent signals from crossing over (stop ctl-c)

        let exitcode = daemonwork(&logpath)?;

        let mut buf = Vec::new();
        write!(&mut buf, "{}", exitcode)?;
        // Ignore PIPE errors (in case the front process was
        // killed by user; the Perl version was simply killed by
        // SIGPIPE before it had a chance to print the error
        // message about not being able to print to sigw):
        let _ = write_all(sigw, &buf);
        let _ = close(sigw);

        unsafe { _exit(0) };
    }
}
