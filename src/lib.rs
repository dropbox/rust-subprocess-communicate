extern crate mio;
extern crate bytes;
mod unix;
#[cfg(unix)]
pub use unix::subprocess_communicate;
