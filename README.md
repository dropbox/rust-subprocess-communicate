#subprocess-communicate
[![crates.io](http://meritbadge.herokuapp.com/subprocess-communicate)](https://crates.io/crates/subprocess-communicate)
[![Build Status](https://travis-ci.org/dropbox/rust-subprocess-communicate.svg?branch=master)](https://travis-ci.org/dropbox/rust-subprocess-communicate)
## Project Requirements
This crate should give an interface to communicating with child processes
similar to python's subprocess.communicate interface from the Popen class.

Pass an input u8 slice and the result should be two Vec<u8>, one for stdout and for stderr.
Also, an error may be returned in case the subprocess pipes were unable to be changed into nonblock mode
or the event loop was unable to be activated.


Unlike the Python interface, this also supports two optional arguments to limit the maximum output size of
stdout and stderr respectively.
This is to prevent a process like `/usr/bin/yes` from consuming all system memory and it
helps reason about maximum resource consumption.

## Usage

```
    let process =
           Command::new("/bin/cat")
           .stdin(Stdio::piped())
           .stdout(Stdio::piped())
           .stderr(Stdio::piped())
           .spawn().unwrap();
     let (ret_stdout, ret_stderr, err) = subprocess_communicate::subprocess_communicate(process, // child subprocess
                                                                                        &TEST_DATA[..], // stdin input
                                                                                        Some(TEST_DATA.len()), // stdout bound
                                                                                        None); // stderr bound (if any)
     err.unwrap();
     assert_eq!(TEST_DATA.len() - 1, ret_stdout.len());
     assert_eq!(0usize, ret_stderr.len());
     let mut i : usize = 0;
     for item in TEST_DATA[0..TEST_DATA.len()].iter() {
         assert_eq!(*item, ret_stdout[i]);
         i += 1;
     }
```
