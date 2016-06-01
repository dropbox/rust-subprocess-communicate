#![cfg(unix)]

extern crate mio;
extern crate bytes;
extern crate nix;
use std::mem;
use mio::*;
use std::io;
use std::process;
use std::cmp;
use mio::unix::{PipeReader, PipeWriter};
#[allow(unused_imports)]
use std::process::{Command, Stdio, Child};

use std::os::unix::io::{AsRawFd, IntoRawFd};
use self::nix::fcntl::FcntlArg::F_SETFL;
use self::nix::fcntl::{fcntl, O_NONBLOCK};

pub fn from_nix_error(err: self::nix::Error) -> io::Error {
    io::Error::from_raw_os_error(err.errno() as i32)
}

fn set_nonblock(s: &AsRawFd) -> io::Result<()> {
    fcntl(s.as_raw_fd(), F_SETFL(O_NONBLOCK)).map_err(from_nix_error)
                                             .map(|_| ())
}


struct SubprocessClient {
    stdin: Option<PipeWriter>,
    stdout: Option<PipeReader>,
    stderr: Option<PipeReader>,
    stdin_token : Token, 
    stdout_token : Token,
    stderr_token : Token,
    output : Vec<u8>,
    output_stderr : Vec<u8>,
    input : Vec<u8>,
    input_offset : usize,
    buf : [u8; 65536],
    stdout_bound : Option<usize>,
    stderr_bound : Option<usize>,
    return_on_stdout_fill : bool,
}


// Sends a message and expects to receive the same exact message, one at a time
impl SubprocessClient {
    fn new(stdin: Option<PipeWriter>, stdout : Option<PipeReader>, stderr : Option<PipeReader>, data : &[u8],
           stdout_bound : Option<usize>, stderr_bound : Option<usize>,
           return_on_stdout_fill : bool) -> SubprocessClient {
        SubprocessClient {
            stdin: stdin,
            stdout: stdout,
            stderr: stderr,
            stdin_token : Token(0),
            stdout_token : Token(1),
            stderr_token : Token(2),
            output : Vec::<u8>::new(),
            output_stderr : Vec::<u8>::new(),
            buf : [0; 65536],
            input : data.to_vec(),
            input_offset : 0,
            stdout_bound : stdout_bound,
            stderr_bound : stderr_bound,
            return_on_stdout_fill : return_on_stdout_fill,
        }
    }

    fn readable(&mut self, event_loop: &mut EventLoop<SubprocessClient>) -> io::Result<()> {
        let mut eof = false;
        let mut buf_bound : usize = cmp::min(self.stdout_bound.unwrap_or(self.buf.len()), self.buf.len());
        if buf_bound == 0 {
            buf_bound = self.buf.len(); // if we ran out of space and our socket is readable, throw result out
        }
        match self.stdout {
            None => unreachable!(),
            Some (ref mut stdout) => match stdout.try_read(&mut self.buf[..buf_bound]) {
                Ok(Some(r)) => {
                    if r == 0 {
                        eof = true;
                    } else {
                        let do_extend : bool;
                        match self.stdout_bound {
                            None => do_extend = true,
                            Some(ref mut bound) => {
                               if *bound >= r {
                                   *bound = *bound - r;
                                  do_extend = true;
                               } else {
                                  *bound = 0;
                                  do_extend = false;
                                  if self.return_on_stdout_fill || self.stderr.is_none() || self.stderr_bound.unwrap_or(1) == 0 {
                                      drop(self.stderr.take());
                                      eof = true;
                                  }
                               }
                            },
                        }
                        if do_extend {
                            self.output.extend(&self.buf[0..r]);
                        }
                    }
                },
                Ok(None) => {},
                Err(e) => {
                    return Err(e);
                }
            }
        };
        if eof {
            drop(self.stdout.take());
            if self.stderr.is_none() {
                event_loop.shutdown();
            }
        }
        return Ok(());
    }

    fn readable_stderr(&mut self, event_loop: &mut EventLoop<SubprocessClient>) -> io::Result<()> {
        let mut eof = false;
        let mut buf_bound : usize = cmp::min(self.stderr_bound.unwrap_or(self.buf.len()), self.buf.len());
        if buf_bound == 0 {
            buf_bound = self.buf.len(); // if we ran out of space and our socket is readable, throw result out
        }
        match self.stderr {
            None => unreachable!(),
            Some(ref mut stderr) => match stderr.try_read(&mut self.buf[..buf_bound]) {
                Ok(None) => {
                }
                Ok(Some(r)) => {
                    if r == 0 {
                        eof = true;
                    } else {
                        let do_extend : bool;
                        match self.stderr_bound {
                            None => do_extend = true,
                            Some(ref mut bound) => {
                               if *bound >= r {
                                  *bound = *bound - r;
                                  do_extend = true;
                               } else {
                                  *bound = 0;
                                  do_extend = false;
                                  if self.stdout.is_none() || self.stdout_bound.unwrap_or(1) == 0 {
                                      drop(self.stdout.take()); // in case stdout had overrun bound
                                      eof = true;
                                  }
                               }
                            },
                        }
                        if do_extend {
                            self.output_stderr.extend(&self.buf[0..r]);
                        }
                    }
                }
                Err(e) => {
                    return Err(e);
                }
            }
        };
        if eof {
            drop(self.stderr.take());
            if self.stdout.is_none() {
                event_loop.shutdown();
            }
        }
        return Ok(());
    }

    fn writable(&mut self, event_loop: &mut EventLoop<SubprocessClient>) -> io::Result<()> {
        let mut ok = true;
        match self.stdin {
            None => unreachable!(),
            Some(ref mut stdin) => match stdin.try_write(&(&self.input)[self.input_offset..]) {
                Ok(None) => {
                },
                Ok(Some(r)) => {
                    if r == 0 {
                        ok = false;
                    } else {
                        self.input_offset += r;
                    }
                },
                Err(_e) => {
                    ok = false;
                },
            }
        }
        if self.input_offset == self.input.len() || !ok {
            drop(self.stdin.take());
            match self.stderr {
                None => match self.stdout {
                            None => event_loop.shutdown(),
                            Some(_) => {},
                },
                Some(_) => {},
            }
        }
        return Ok(());
    }

}

impl Handler for SubprocessClient {
    type Timeout = usize;
    type Message = ();

    fn ready(&mut self, event_loop: &mut EventLoop<SubprocessClient>, token: Token,
             _events: EventSet) {
        //println!("ready {:?} {:?} {:}", token, events, events.is_readable());
        if token == self.stderr_token {
            let _x = self.readable_stderr(event_loop);
        } else {
            let _x = self.readable(event_loop);
        }
        if token == self.stdin_token {
            let _y = self.writable(event_loop);
        }
    }
}

#[cfg(not(feature = "mio-stdio"))]
pub fn from_stdin(mut stdin: Option<process::ChildStdin>) -> io::Result<Option<PipeWriter>> {
    match stdin {
      None => return Ok(None),
      Some(_) => {},
    }
    let local_stdin = stdin.take().unwrap();
    match set_nonblock(&local_stdin) {
        Err(e) => return Err(e),
        _ => {},
    }
    return Ok(Some(PipeWriter::from(Io::from_raw_fd(local_stdin.into_raw_fd()))));
}

#[cfg(feature = "mio-stdio")]
pub fn from_stdin(mut stdin: Option<process::ChildStdin>) -> io::Result<Option<PipeWriter> > {
    match process.stdin {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeWriter::from_stdin(process.stdin.take().unwrap())))
}

#[cfg(not(feature = "mio-stdio"))]
pub fn from_stdout(mut stdout: Option<process::ChildStdout>) -> io::Result<Option<PipeReader>> {
    match stdout {
      None => return Ok(None),
      Some(_) => {},
    }
    let local_stdout = stdout.take().unwrap();
    match set_nonblock(&local_stdout) {
        Err(e) => return Err(e),
        _ => {},
    }
    return Ok(Some(PipeReader::from(Io::from_raw_fd(local_stdout.into_raw_fd()))));
}

#[cfg(feature = "mio-stdio")]
pub fn from_stdout(mut stdout: Option<process::ChildStdout>) -> io::Result<Option<PipeWriter> > {
    match process.stdout {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeReader::from_stdout(process.stdout.take().unwrap())))
}

#[cfg(not(feature = "mio-stdio"))]
pub fn from_stderr(mut stderr: Option<process::ChildStderr>) -> io::Result<Option<PipeReader>> {
    match stderr {
      None => return Ok(None),
      Some(_) => {},
    }
    let local_stderr = stderr.take().unwrap();
    match set_nonblock(&local_stderr) {
        Err(e) => return Err(e),
        _ => {},
    }
    return Ok(Some(PipeReader::from(Io::from_raw_fd(local_stderr.into_raw_fd()))));
}

#[cfg(feature = "mio-stdio")]
pub fn from_stderr(mut stderr: Option<process::ChildStderr>) -> io::Result<Option<PipeWriter> > {
    match process.stderr {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeReader::from_stderr(process.stderr.take().unwrap())))
}

/// Sends input to process and returns stdout and stderr
/// up until stdout_bound or stderr_bound are reached
/// If stdout_bound is reached and return_on_stdout_fill is true,
/// the rest of stderr will not be awaited
///
/// Conversely, if stdout_bound is reached and return_on_stderr_fill is false
/// Then if insufficient stderr is produced and that file descriptor is not closed by
/// the callee, then the subprocess_communicate will hang until the child produces
/// up to at least the stderr_bound or closes the stderr file descriptor
/// This function may return errors if the stdin, stdout or stderr are unable to be set into nonblocking
/// or if the event loop is unable to be created, otherwise the last return value will be Ok(())
pub fn subprocess_communicate(process : &mut Child,
                              input : &[u8],
                              stdout_bound : Option<usize>,
                              stderr_bound : Option<usize>,
                              return_on_stdout_fill : bool) -> (Vec<u8>, Vec<u8>, io::Result<()>) {
    let event_loop_result = EventLoop::<SubprocessClient>::new();
    match event_loop_result {
        Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
        Ok(_) => {},
    }
    let mut event_loop = event_loop_result.unwrap();
    let stdin : Option<PipeWriter>;
    match from_stdin(process.stdin.take()) {
        Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
        Ok(pipe) => stdin = pipe,
    }

    let stdout : Option<PipeReader>;
    match from_stdout(process.stdout.take()) {
        Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
        Ok(pipe) => stdout = pipe,
    }

    let stderr : Option<PipeReader>;
    match from_stderr(process.stderr.take()) {
        Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
        Ok(pipe) => stderr = pipe,
    }


    //println!("listen for connections {:?} {:?}", , process.stdout.unwrap().as_raw_fd());
    let mut subprocess = SubprocessClient::new(stdin,
                                               stdout,
                                               stderr,
                                               input,
                                               stdout_bound,
                                               stderr_bound,
                                               return_on_stdout_fill);
    match subprocess.stdout {
       Some(ref sub_stdout) =>
          match event_loop.register(sub_stdout, subprocess.stdout_token, EventSet::readable(),
                                                   PollOpt::level()) {
            Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
            Ok(_) =>{},
          },
       None => {},
    }

    match subprocess.stderr {
        Some(ref sub_stderr) => match event_loop.register(sub_stderr, subprocess.stderr_token, EventSet::readable(),
                        PollOpt::level()) {
           Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
           Ok(_) => {},
        },
        None => {},
    }

    // Connect to the server
    match subprocess.stdin {
        Some (ref sub_stdin) => match event_loop.register(sub_stdin, subprocess.stdin_token, EventSet::writable(),
                        PollOpt::level()) {
           Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
           Ok(_) => {},
         },
         None => {},
    }

    // Start the event loop
    event_loop.run(&mut subprocess).unwrap();
    //let res = process.wait();

    let ret_stdout = mem::replace(&mut subprocess.output, Vec::<u8>::new());
    let ret_stderr = mem::replace(&mut subprocess.output_stderr, Vec::<u8>::new());

    return (ret_stdout, ret_stderr, Ok(()));
}

#[allow(dead_code)]
const TEST_DATA : [u8; 1024 * 4096] = [42; 1024 * 4096];

#[test]
fn test_subprocess_pipe() {
    let mut process =
           Command::new("/bin/cat")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::piped())
           .spawn().unwrap();
     let (ret_stdout, ret_stderr, err) = subprocess_communicate(&mut process, &TEST_DATA[..], None, None, true);
     process.wait().unwrap();
     err.unwrap();
     assert_eq!(TEST_DATA.len(), ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in TEST_DATA.iter() {
         assert_eq!(*item, ret_stdout[i]);
         i += 1;
     }
}


#[test]
fn test_subprocess_bounded_pipe() {
    let mut process =
           Command::new("/bin/cat")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::piped())
           .spawn().unwrap();
     let (ret_stdout, ret_stderr, err) = subprocess_communicate(&mut process, &TEST_DATA[..], Some(TEST_DATA.len() - 1), None, true);
     process.wait().unwrap();
     err.unwrap();
     assert_eq!(TEST_DATA.len() - 1, ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in TEST_DATA[0..TEST_DATA.len() - 1].iter() {
         assert_eq!(*item, ret_stdout[i]);
         i += 1;
     }
}

#[test]
fn test_subprocess_bounded_yes_stderr0() {
    let mut process =
           Command::new("/usr/bin/yes")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::piped())
           .spawn().unwrap();
     let bound : usize = 130000;
     let (ret_stdout, ret_stderr, err) = subprocess_communicate(&mut process, &TEST_DATA[..], Some(bound), Some(0), false);
     err.unwrap();
     assert_eq!(bound, ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in ret_stdout.iter() {
         let val : u8;
         if (i & 1) == 1 {
             val = '\n' as u8;
         } else {
             val = 'y' as u8;
         }
         assert_eq!(*item, val);
         i += 1;
     }
}

#[test]
fn test_subprocess_bounded_yes() {
    let mut process =
           Command::new("/usr/bin/yes")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::piped())
           .spawn().unwrap();
     let bound : usize = 130000;
     let (ret_stdout, ret_stderr, err) = subprocess_communicate(&mut process, &TEST_DATA[..], Some(bound), Some(bound), true);
     err.unwrap();
     assert_eq!(bound, ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in ret_stdout.iter() {
         let val : u8;
         if (i & 1) == 1 {
             val = '\n' as u8;
         } else {
             val = 'y' as u8;
         }
         assert_eq!(*item, val);
         i += 1;
     }
}


#[test]
fn test_subprocess_bounded_yes_no_stderr() {
    let mut process =
           Command::new("/usr/bin/yes")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .spawn().unwrap();
     let bound : usize = 130000;
     let (ret_stdout, ret_stderr, err) = subprocess_communicate(&mut process, &TEST_DATA[..], Some(bound), None, false);
     err.unwrap();
     assert_eq!(bound, ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in ret_stdout.iter() {
         let val : u8;
         if (i & 1) == 1 {
             val = '\n' as u8;
         } else {
             val = 'y' as u8;
         }
         assert_eq!(*item, val);
         i += 1;
     }
}
