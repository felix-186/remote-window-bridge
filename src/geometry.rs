use crate::error::{DesktopError, Result};
use serde::{Deserialize, Serialize};

/// Physical pixels. Platform adapters must convert logical coordinates at their boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub fn validate(self) -> Result<Self> {
        if self.width == 0 || self.height == 0 || self.width > 32768 || self.height > 32768 {
            return Err(DesktopError::invalid(
                "Rectangle dimensions must be in 1..=32768",
            ));
        }
        if self.right() > i32::MAX as i64 || self.bottom() > i32::MAX as i64 {
            return Err(DesktopError::invalid(
                "Rectangle overflows physical coordinates",
            ));
        }
        Ok(self)
    }
    pub fn right(self) -> i64 {
        self.x as i64 + self.width as i64
    }
    pub fn bottom(self) -> i64 {
        self.y as i64 + self.height as i64
    }
    pub fn contains(self, x: i32, y: i32) -> bool {
        x as i64 >= self.x as i64
            && y as i64 >= self.y as i64
            && (x as i64) < self.right()
            && (y as i64) < self.bottom()
    }
    pub fn intersects(self, other: Self) -> bool {
        (self.x as i64) < other.right()
            && (other.x as i64) < self.right()
            && (self.y as i64) < other.bottom()
            && (other.y as i64) < self.bottom()
    }
    pub fn crop(self, relative: Rect) -> Result<Rect> {
        relative.validate()?;
        if relative.x < 0
            || relative.y < 0
            || relative.right() > self.width as i64
            || relative.bottom() > self.height as i64
        {
            return Err(DesktopError::invalid(
                "Viewport must be inside the client rectangle",
            ));
        }
        Rect {
            x: self
                .x
                .checked_add(relative.x)
                .ok_or_else(|| DesktopError::invalid("x overflow"))?,
            y: self
                .y
                .checked_add(relative.y)
                .ok_or_else(|| DesktopError::invalid("y overflow"))?,
            width: relative.width,
            height: relative.height,
        }
        .validate()
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    #[default]
    ImagePixels,
    Normalized,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Point {
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub coordinate_space: CoordinateSpace,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct ScreenPoint {
    pub x: i32,
    pub y: i32,
}

/// Maps a returned image to a calibrated content rectangle; no remote resolution required.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ImageTransform {
    pub image_width: u32,
    pub image_height: u32,
    pub content_screen: Rect,
}

impl ImageTransform {
    pub fn map(self, point: Point) -> Result<ScreenPoint> {
        self.content_screen.validate()?;
        if self.image_width == 0
            || self.image_height == 0
            || !point.x.is_finite()
            || !point.y.is_finite()
        {
            return Err(DesktopError::invalid(
                "Invalid image size or non-finite point",
            ));
        }
        let (x, y) = match point.coordinate_space {
            CoordinateSpace::ImagePixels => (point.x, point.y),
            CoordinateSpace::Normalized => {
                if !(0.0..=1.0).contains(&point.x) || !(0.0..=1.0).contains(&point.y) {
                    return Err(DesktopError::invalid(
                        "Normalized coordinates must be in [0,1]",
                    ));
                }
                (
                    point.x * (self.image_width - 1) as f64,
                    point.y * (self.image_height - 1) as f64,
                )
            }
        };
        if x < 0.0
            || y < 0.0
            || x > (self.image_width - 1) as f64
            || y > (self.image_height - 1) as f64
        {
            return Err(DesktopError::invalid(
                "Point is outside the referenced image",
            ));
        }
        let sx = self.content_screen.x as f64
            + (x + 0.5) * self.content_screen.width as f64 / self.image_width as f64
            - 0.5;
        let sy = self.content_screen.y as f64
            + (y + 0.5) * self.content_screen.height as f64 / self.image_height as f64
            - 0.5;
        let p = ScreenPoint {
            x: (sx.round() as i32).clamp(
                self.content_screen.x,
                (self.content_screen.right() - 1) as i32,
            ),
            y: (sy.round() as i32).clamp(
                self.content_screen.y,
                (self.content_screen.bottom() - 1) as i32,
            ),
        };
        Ok(p)
    }
}

pub fn absolute_mouse(point: ScreenPoint, desktop: Rect) -> Result<(i32, i32)> {
    desktop.validate()?;
    if !desktop.contains(point.x, point.y) || desktop.width < 2 || desktop.height < 2 {
        return Err(DesktopError::invalid(
            "Point is outside the virtual desktop",
        ));
    }
    Ok((
        ((point.x as i64 - desktop.x as i64) as f64 * 65535.0 / (desktop.width - 1) as f64).round()
            as i32,
        ((point.y as i64 - desktop.y as i64) as f64 * 65535.0 / (desktop.height - 1) as f64).round()
            as i32,
    ))
}
