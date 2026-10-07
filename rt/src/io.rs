use crate::rpc::Service;
use crate::{sys, String};
use core::fmt;

const CONSOLE_READ: u16 = 2;

// formats the whole thing first so one print is one piece of output
pub fn _print(args: fmt::Arguments) {
    match args.as_str() {
        Some(s) => sys::print(s.as_bytes()),
        None => sys::print(alloc::fmt::format(args).as_bytes()),
    }
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::io::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::io::_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::io::_print(format_args!("{}\n", format_args!($($arg)*))));
}

// a line typed at the console, without its newline. None at end of input
// (ctrl-d on an empty line) or when there is no console.
pub fn read_line() -> Option<String> {
    let console = Service::lookup("console").ok()?;
    let mut line = alloc::vec::Vec::new();
    loop {
        let p = console.call_forever(CONSOLE_READ, &[]).ok()?;
        if p.len == 0 {
            return None;
        }
        line.extend_from_slice(p.bytes());
        if line.last() == Some(&b'\n') {
            line.pop();
            return Some(String::from_utf8_lossy(&line).into());
        }
    }
}
