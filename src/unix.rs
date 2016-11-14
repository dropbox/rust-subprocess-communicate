#![cfg(unix)]

extern crate mio;
use std::mem;
use mio::*;
use std::io;
use std::process;
use std::cmp;

use mio::deprecated::{TryRead, TryWrite};
use mio::deprecated::{PipeReader, PipeWriter};
#[allow(unused_imports)]
use std::process::{Command, Stdio, Child};


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
    has_shutdown : bool,
    child_shutdown : bool,
}


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
            has_shutdown : false,
            child_shutdown : false,
        }
    }

    fn readable(&mut self, poll: &mut Poll) -> io::Result<()> {
        if self.has_shutdown {
            return Ok(());
        }
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
                                      match self.stderr {
                                          Some(ref sub_stderr) =>
                                              match poll.deregister(sub_stderr){
                                                Err(e) => return Err(e),
                                                _ => {},
                                          },
                                          _ => {},
                                      }
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
            match self.stdout {
               Some(ref sub_stdout) =>
                   match poll.deregister(sub_stdout) {
                      Err(e) => return Err(e),
                      _ => {},
                   },
               _ => {},
            }
            drop(self.stdout.take());
            if self.stderr.is_none() {
                self.has_shutdown = true;
                self.child_shutdown = true;
            }
        }
        return Ok(());
    }

    fn readable_stderr(&mut self, poll: &mut Poll) -> io::Result<()> {
        if self.has_shutdown {
            return Ok(());
        }

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
                                      match self.stdout {
                                          Some(ref sub_stdout) =>
                                              match poll.deregister(sub_stdout){
                                                  Err(e) => return Err(e),
                                                  _ => {},
                                              },
                                          _ => {},
                                      }
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
            match self.stderr {
               Some(ref sub_stderr) =>
                   match poll.deregister(sub_stderr){
                       Err(e) => return Err(e),
                       _ => {},
                   },
               _ => {},
            }
            drop(self.stderr.take());
            if self.stdout.is_none() {
                self.has_shutdown = true;
                self.child_shutdown = true;
            }
        }
        return Ok(());
    }

    fn writable(&mut self, poll: &mut Poll) -> io::Result<()> {
        if self.has_shutdown {
            return Ok(());
        }

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
            match self.stdin {
                Some(ref sub_stdin) =>
                    match poll.deregister(sub_stdin) {
                       Err(e) => return Err(e),
                       _ => {},
                    },
                _ => {},
            }
            drop(self.stdin.take());
            match self.stderr {
                None => match self.stdout {
                            None => {
                                self.has_shutdown = true;
                                self.child_shutdown = true
                            },
                            Some(_) => {},
                },
                Some(_) => {},
            }
        }
        return Ok(());
    }

    fn ready(&mut self, poll: &mut Poll, token: Token,
             _events: Ready) {
        if token == self.stderr_token {
            let _x = self.readable_stderr(poll);
        } else {
            let _x = self.readable(poll);
        }
        if token == self.stdin_token {
            let _y = self.writable(poll);
        }
    }
}

pub fn from_stdin(mut stdin: Option<process::ChildStdin>) -> io::Result<Option<PipeWriter> > {
    match stdin {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeWriter::from_stdin(stdin.take().unwrap()).unwrap()))
}

pub fn from_stdout(mut stdout: Option<process::ChildStdout>) -> io::Result<Option<PipeReader> > {
    match stdout {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeReader::from_stdout(stdout.take().unwrap()).unwrap()))
}


pub fn from_stderr(mut stderr: Option<process::ChildStderr>) -> io::Result<Option<PipeReader> > {
    match stderr {
      None => return Ok(None),
      Some(_) => {},
    }
    Ok(Some(PipeReader::from_stderr(stderr.take().unwrap()).unwrap()))
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


    let mut subprocess = SubprocessClient::new(stdin,
                                               stdout,
                                               stderr,
                                               input,
                                               stdout_bound,
                                               stderr_bound,
                                               return_on_stdout_fill);
    let mut poll = Poll::new().unwrap();
    match subprocess.stdout {
       Some(ref sub_stdout) =>
          match poll.register(sub_stdout, subprocess.stdout_token, Ready::readable(),
                                                   PollOpt::level()) {
            Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
            Ok(_) =>{},
          },
       None => {},
    }

    match subprocess.stderr {
        Some(ref sub_stderr) => match poll.register(sub_stderr, subprocess.stderr_token, Ready::readable(),
                        PollOpt::level()) {
           Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
           Ok(_) => {},
        },
        None => {},
    }

    // Connect to the server
    match subprocess.stdin {
        Some (ref sub_stdin) => match poll.register(sub_stdin, subprocess.stdin_token, Ready::writable(),
                        PollOpt::level()) {
           Err(e) => return (Vec::<u8>::new(), Vec::<u8>::new(), Err(e)),
           Ok(_) => {},
         },
         None => {},
    }
    let mut events = Events::with_capacity(1024);
    while !subprocess.child_shutdown {
        poll.poll(&mut events, None).unwrap();
        for event in events.iter() {
            subprocess.ready(&mut poll, event.token(), event.kind())
        }
    }

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
