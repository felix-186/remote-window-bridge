use super::*;
impl CaptureBackend for WindowsPlatform {
    fn capture(&self, snapshot: &WindowSnapshot, cancel: &Cancellation) -> Result<RgbImage> {
        cancel.check()?;
        ready(snapshot)?;
        unsafe {
            let area = snapshot.content_screen;
            let len = area.width as u64 * area.height as u64 * 4;
            if len > 64 * 1024 * 1024 {
                return Err(DesktopError::new(
                    "FRAME_TOO_LARGE",
                    "Calibrate a viewport smaller than 16 million pixels",
                ));
            }
            let mut resources = CaptureResources {
                screen: GetDC(null_mut()),
                memory: null_mut(),
                bitmap: null_mut(),
                previous: null_mut(),
            };
            if resources.screen.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "GetDC"));
            }
            resources.memory = CreateCompatibleDC(resources.screen);
            if resources.memory.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "CreateCompatibleDC"));
            }
            let mut bmi: BITMAPINFO = zeroed();
            bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
            bmi.bmiHeader.biWidth = area.width as i32;
            bmi.bmiHeader.biHeight = -(area.height as i32); // top-down BGRA
            bmi.bmiHeader.biPlanes = 1;
            bmi.bmiHeader.biBitCount = 32;
            bmi.bmiHeader.biCompression = BI_RGB;
            let mut pixels = null_mut();
            resources.bitmap = CreateDIBSection(
                resources.screen,
                &bmi,
                DIB_RGB_COLORS,
                &mut pixels,
                null_mut(),
                0,
            );
            if resources.bitmap.is_null() || pixels.is_null() {
                return Err(native_error("CAPTURE_UNAVAILABLE", "CreateDIBSection"));
            }
            resources.previous = SelectObject(resources.memory, resources.bitmap);
            if resources.previous.is_null() || resources.previous as isize == -1 {
                return Err(native_error("CAPTURE_UNAVAILABLE", "SelectObject"));
            }
            if BitBlt(
                resources.memory,
                0,
                0,
                area.width as i32,
                area.height as i32,
                resources.screen,
                area.x,
                area.y,
                SRCCOPY | CAPTUREBLT,
            ) == 0
            {
                return Err(native_error("CAPTURE_UNAVAILABLE", "BitBlt"));
            }
            if GdiFlush() == 0 {
                return Err(native_error("CAPTURE_UNAVAILABLE", "GdiFlush"));
            }
            let raw = std::slice::from_raw_parts(pixels as *const u8, len as usize);
            let mut rgb = Vec::with_capacity(area.width as usize * area.height as usize * 3);
            for pixel in raw.chunks_exact(4) {
                rgb.extend([pixel[2], pixel[1], pixel[0]]);
            }
            cancel.check()?;
            ready(snapshot)?;
            Ok(RgbImage {
                width: area.width,
                height: area.height,
                pixels: rgb,
            })
        }
    }
}

struct CaptureResources {
    screen: HDC,
    memory: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
}
impl Drop for CaptureResources {
    fn drop(&mut self) {
        unsafe {
            if !self.previous.is_null() && self.previous as isize != -1 && !self.memory.is_null() {
                SelectObject(self.memory, self.previous);
            }
            if !self.bitmap.is_null() {
                DeleteObject(self.bitmap);
            }
            if !self.memory.is_null() {
                DeleteDC(self.memory);
            }
            if !self.screen.is_null() {
                ReleaseDC(null_mut(), self.screen);
            }
        }
    }
}
