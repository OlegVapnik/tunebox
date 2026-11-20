
use std::io;

use crate::os;

pub fn available() -> bool {
    os::secrets::AVAILABLE
}

pub fn get(name: &str) -> io::Result<Option<Vec<u8>>> {
    os::secrets::get(name)
}

pub fn set(name: &str, secret: &[u8]) -> io::Result<()> {
    os::secrets::set(name, secret)
}

pub fn delete(name: &str) -> io::Result<()> {
    os::secrets::delete(name)
}
