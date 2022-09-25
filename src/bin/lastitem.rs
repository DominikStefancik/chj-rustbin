#[macro_use]
extern crate log; // `tracing` crate, huh?

use anyhow::{Result, bail, anyhow};
use std::fs;
use std::io;
use std::env;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::ffi::OsString;
use std::io::Write;
use std::time::SystemTime;
use std::path::PathBuf;
use structopt::StructOpt;
use rayon::iter::ParallelIterator;
use rayon::iter::IntoParallelIterator;


#[derive(StructOpt, Debug)]
/// Show the newest (with regards to mtime) item in a directory. If
/// called via a symlink as `lastfile`, shows the last file, if called
/// as `lastdir`, the last dir, if called as `lastitem`, either file
/// or dir. Alternatively, if the --dirs or --files option is given,
/// that takes precedence. Always ignores device, pipe and socket
/// files.
#[structopt(name = "lastitem from chj-rustbin")]
struct Opt {
    /// consider dirs
    #[structopt(long)]
    dirs: bool,

    /// consider files
    #[structopt(long)]
    files: bool,

    /// show the full path instead of just the filename
    #[structopt(short, long)]
    fullpath: bool,

    /// the directory to find the item in
    #[structopt(parse(from_os_str), default_value = ".")]
    directory_path: PathBuf,
}

struct Item {
    path: PathBuf,
    mtime: SystemTime,
}

fn item_merge(old_item: Option<Item>, new_item: Option<Item>)
              -> Option<Item> {
    match new_item {
        Some(Item { path: ref new_path, mtime: new_mtime }) =>
            match old_item {
                Some(Item { path: ref old_path, mtime: old_mtime }) =>
                    if (old_mtime < new_mtime)
                    || ((old_mtime == new_mtime) &&
                        (*old_path > *new_path)) {
                        new_item
                    } else {
                        old_item
                    }
                None =>
                    new_item
            },
        None =>
            old_item
    }
}

fn main() -> Result<()> {
    let mut opt = Opt::from_args();

    if !opt.files && !opt.dirs {
        let arg0 = env::args_os().next();
        let exepath = arg0.ok_or(
            anyhow!("can't get executable path from args_os")
        )?;
        let exename = <OsString as AsRef<Path>>::as_ref(&exepath).file_name()
            .ok_or(
                anyhow!("can't extract file_name from executable path")
            )?;

        if exename == "lastitem" {
            opt.files = true;
            opt.dirs = true;
        } else if exename == "lastfile" {
            opt.files = true;
        } else if exename == "lastdir" {
            opt.dirs = true;
        } else {
            bail!("inacceptable executable name: {}",
                  exename.to_string_lossy());
        }
    }

    let items: Vec<fs::DirEntry> =
        fs::read_dir(&opt.directory_path)?.collect::<Result<_,_>>()?;
    let newest_item =
        items.into_par_iter().try_fold(
            || None, // : Option<Item>,
            |newest_item: Option<Item>, entry: fs::DirEntry| {
                let itempath = entry.path();
                let md = fs::symlink_metadata(&itempath)?;
                let mtime = md.modified()?;

                let keep_if_newer =
                    |itempath, newest_item| -> Result<Option<Item>> {
                        Ok(item_merge(
                            newest_item,
                            Some(Item { path: itempath, mtime: mtime })))
                    };

                if md.is_dir() && opt.dirs {
                    keep_if_newer(itempath, newest_item)
                } else if md.is_file() && opt.files {
                    keep_if_newer(itempath, newest_item)
                } else {
                    trace!("ignoring path '{:?}' (type {:?})",
                           &itempath, md.file_type());
                    Ok(newest_item)
                }
            }).try_reduce(
            || None,
            |a, b| Ok(item_merge(a, b)))?;

    match newest_item {
        Some(Item { path, mtime: _ }) => {
            io::stdout().write_all(
                if opt.fullpath {
                    path.as_os_str()
                } else {
                    path.file_name()
                        .expect("should never see .. as file_name")
                }.as_bytes())?;
            io::stdout().write_all(
                "\n".as_bytes())
        }
        None =>
            bail!("No {} found in given directory",
                  if opt.dirs {
                      if opt.files {
                          "directory or file"
                      } else {
                          "directory"
                      }
                  } else if opt.files {
                      "file"
                  } else {
                      panic!("neither option is set")
                  })
    }?;

    Ok(())
}
