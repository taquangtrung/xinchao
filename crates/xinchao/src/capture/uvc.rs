//! Low-level UVC extension-unit (XU) control access.
//!
//! Some UVC cameras gate their IR illuminator behind a vendor-specific extension
//! unit: the emitter only turns on after the host issues a `SET_CUR` request to a
//! particular unit/selector with a device-specific payload (the approach taken by
//! [`linux-enable-ir-emitter`](https://github.com/EmixaM23/linux-enable-ir-emitter)).
//!
//! This module is the thin, `unsafe` ioctl layer: it wraps the kernel's
//! `UVCIOC_CTRL_QUERY` so higher layers can read a control's length and range
//! (safe, read-only) and, deliberately, write a payload to it.
//!
//! # Safety and risk
//!
//! Writing arbitrary payloads to vendor controls is the one genuinely risky thing
//! xinchao does to hardware; a bad value can wedge a camera until it is replugged.
//! Callers should write only values learned from the device's own range queries.

use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::mem;
use std::os::fd::AsRawFd;
use std::os::raw::c_ulong;
use std::os::raw::c_void;
use std::path::Path;

use crate::error::Error;
use crate::error::Result;

// Constants

/// UVC request: set the current value (the only write we issue).
const UVC_SET_CUR: u8 = 0x01;
/// UVC request: get the current value.
const UVC_GET_CUR: u8 = 0x81;
/// UVC request: get the minimum value.
const UVC_GET_MIN: u8 = 0x82;
/// UVC request: get the maximum value.
const UVC_GET_MAX: u8 = 0x83;
/// UVC request: get the default value.
const UVC_GET_DEF: u8 = 0x87;
/// UVC request: get the control's byte length.
const UVC_GET_LEN: u8 = 0x85;

/// `_IOC` direction bits for a read+write ioctl.
const IOC_READ_WRITE: c_ulong = 3;
/// `_IOC` type ('u') for the uvcvideo ioctls.
const IOC_TYPE: c_ulong = b'u' as c_ulong;
/// `_IOC` number for `UVCIOC_CTRL_QUERY`.
const IOC_NR_CTRL_QUERY: c_ulong = 0x21;

// Data Structures

/// A snapshot of one XU control's length and read-only value range.
#[derive(Clone, Debug)]
pub struct ControlSnapshot {
    /// Current value (`GET_CUR`).
    pub cur: Vec<u8>,
    /// Default value (`GET_DEF`).
    pub def: Vec<u8>,
    /// Control length in bytes (`GET_LEN`).
    pub len: u16,
    /// Maximum value (`GET_MAX`).
    pub max: Vec<u8>,
    /// Minimum value (`GET_MIN`).
    pub min: Vec<u8>,
}

/// An open video node for issuing UVC extension-unit control queries.
pub struct Handle {
    file: File,
}

/// Mirror of the kernel's `struct uvc_xu_control_query`.
///
/// Field order is fixed by the C ABI and must not be reordered or sorted.
#[repr(C)]
struct XuControlQuery {
    unit: u8,
    selector: u8,
    query: u8,
    size: u16,
    data: *mut u8,
}

// Functions

/// Computes the `UVCIOC_CTRL_QUERY` ioctl request number for this target.
///
/// Equivalent to the C macro `_IOWR('u', 0x21, struct uvc_xu_control_query)`;
/// the encoded size depends on pointer width, so it is derived at runtime.
fn request_code() -> c_ulong {
    let size = mem::size_of::<XuControlQuery>() as c_ulong;
    (IOC_READ_WRITE << 30) | (size << 16) | (IOC_TYPE << 8) | IOC_NR_CTRL_QUERY
}

impl Handle {
    /// Opens a video node read+write for control queries.
    pub fn open(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| {
                Error::Camera(format!(
                    "cannot open {} for UVC control: {e}",
                    path.display()
                ))
            })?;
        Ok(Handle { file })
    }

    /// Reads a control's byte length (`GET_LEN`).
    pub fn control_len(&self, unit: u8, selector: u8) -> Result<u16> {
        let mut buf = [0u8; 2];
        self.query(unit, selector, UVC_GET_LEN, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    /// Reads a control value with the given request code into a `size`-byte buffer.
    pub fn get(&self, unit: u8, selector: u8, query: u8, size: u16) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; size as usize];
        self.query(unit, selector, query, &mut buf)?;
        Ok(buf)
    }

    /// Writes a payload to a control (`SET_CUR`). This is the risky operation.
    pub fn set_cur(&self, unit: u8, selector: u8, data: &[u8]) -> Result<()> {
        let mut buf = data.to_vec();
        self.query(unit, selector, UVC_SET_CUR, &mut buf)
    }

    /// Reads a control's length and full value range, best effort per field.
    ///
    /// Length is required; the individual range queries are tolerant, leaving a
    /// field empty when the device refuses that particular request.
    pub fn snapshot(&self, unit: u8, selector: u8) -> Result<ControlSnapshot> {
        let len = self.control_len(unit, selector)?;
        Ok(ControlSnapshot {
            cur: self
                .get(unit, selector, UVC_GET_CUR, len)
                .unwrap_or_default(),
            def: self
                .get(unit, selector, UVC_GET_DEF, len)
                .unwrap_or_default(),
            len,
            max: self
                .get(unit, selector, UVC_GET_MAX, len)
                .unwrap_or_default(),
            min: self
                .get(unit, selector, UVC_GET_MIN, len)
                .unwrap_or_default(),
        })
    }

    /// Issues one `UVCIOC_CTRL_QUERY` ioctl against the open node.
    fn query(&self, unit: u8, selector: u8, query: u8, data: &mut [u8]) -> Result<()> {
        let mut control = XuControlQuery {
            unit,
            selector,
            query,
            size: data.len() as u16,
            data: data.as_mut_ptr(),
        };
        let ret = unsafe {
            libc::ioctl(
                self.file.as_raw_fd(),
                request_code(),
                &mut control as *mut _ as *mut c_void,
            )
        };
        if ret < 0 {
            return Err(Error::Camera(format!(
                "UVC query failed (unit {unit}, selector {selector}, request {query:#04x}): {}",
                io::Error::last_os_error()
            )));
        }
        Ok(())
    }
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn request_code_matches_kernel_macro() {
        // _IOWR('u', 0x21, struct uvc_xu_control_query) on 64-bit.
        assert_eq!(request_code(), 0xC010_7521);
    }

    #[test]
    fn control_query_has_c_abi_size() {
        // unit/selector/query (3) + pad (1) + size (2) + pad (2) + ptr (8).
        #[cfg(target_pointer_width = "64")]
        assert_eq!(mem::size_of::<XuControlQuery>(), 16);
    }
}
