extern crate mio;
mod unix;
#[cfg(unix)]
pub use unix::subprocess_communicate;
