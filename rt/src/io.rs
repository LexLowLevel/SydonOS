use crate::sys;
use core::fmt;

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
