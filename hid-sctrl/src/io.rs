use hidapi::{HidDevice, HidError};

pub fn hid_get_input_report<'a>(
    hid_device: &mut HidDevice,
    buf: &'a mut [u8],
) -> Result<Option<&'a [u8]>, HidError> {
    match hid_device.read_timeout(buf, 0) {
        Ok(read_len) if read_len > 0 => Ok(Some(&buf[..read_len])),
        Ok(_) => Ok(None),
        Err(err) => Err(err),
    }
}

pub fn hid_set_output_report(hid_device: &mut HidDevice, buf: &[u8]) -> Result<(), HidError> {
    match hid_device.write(buf) {
        Ok(_) => Ok(()),
        Err(err) => Err(err),
    }
}
