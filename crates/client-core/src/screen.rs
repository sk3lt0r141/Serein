//! Bounded screen-share settings and ephemeral Gateway negotiation events.
use crate::voice;
use model::Id;

pub const MAX_SOURCES: usize = 64;
pub const MAX_SOURCE_NAME_BYTES: usize = 256;
/// Wide displays share a 4K pixel budget, rather than a 16:9 bounding box.
pub const MAX_VIDEO_WIDTH: u32 = 7680;
pub const MAX_VIDEO_HEIGHT: u32 = 4320;
pub const MAX_VIDEO_PIXELS: u64 = 3840 * 2160;

pub fn valid_dimensions(width: u32, height: u32) -> bool {
	width > 0
		&& height > 0
		&& width <= MAX_VIDEO_WIDTH
		&& height <= MAX_VIDEO_HEIGHT
		&& u64::from(width) * u64::from(height) <= MAX_VIDEO_PIXELS
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceId {
	/// macOS chooses a display/window only after an explicit Share action.
	SystemPicker,
	/// The Linux desktop chooses the source after an explicit Share action.
	Portal,
	/// Explicit whole-desktop capture on a native X11 session, without a portal.
	X11Desktop,
	Display(u64),
	Window(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
	pub id: SourceId,
	pub name: String,
	/// Physical pixels when discovery knows them; system pickers resolve later.
	pub dimensions: Option<(u32, u32)>,
}

impl Source {
	/// Height zero requests native size. Other presets scale without enlarging the source.
	pub fn output_dimensions(&self, height: u32) -> Option<(u32, u32)> {
		if !matches!(height, 0 | 480 | 720 | 1080 | 1440 | 2160) {
			return None;
		}
		let Some((width, source_height)) = self.dimensions else {
			return match height {
				480 => Some((854, 480)),
				720 => Some((1280, 720)),
				1080 => Some((1920, 1080)),
				1440 => Some((2560, 1440)),
				2160 => Some((3840, 2160)),
				_ => None,
			};
		};
		if width < 2 || source_height < 2 {
			return None;
		}
		let scale = if height == 0 {
			1.0
		} else {
			(f64::from(height) / f64::from(source_height))
				.min(1.0)
				.min(f64::from(MAX_VIDEO_WIDTH) / f64::from(width))
				.min(f64::from(MAX_VIDEO_HEIGHT) / f64::from(source_height))
				.min(
					(MAX_VIDEO_PIXELS as f64 / (f64::from(width) * f64::from(source_height)))
						.sqrt(),
				)
		};
		// H.264 4:2:0 needs even dimensions. Round down by at most one pixel per edge.
		let size = (
			((f64::from(width) * scale) as u32 / 2 * 2).max(2),
			((f64::from(source_height) * scale) as u32 / 2 * 2).max(2),
		);
		valid_dimensions(size.0, size.1).then_some(size)
	}
}

#[derive(Clone, Copy, Debug)]
pub struct Settings {
	pub source: SourceId,
	pub width: u32,
	pub height: u32,
	pub fps: u32,
	pub cursor: bool,
	/// Share system audio with the screen; call microphone settings are independent.
	pub audio: bool,
}
impl Settings {
	pub fn valid(self) -> bool {
		!matches!(self.source, SourceId::Display(0) | SourceId::Window(0))
			&& valid_dimensions(self.width, self.height)
			&& self.width.is_multiple_of(2)
			&& self.height.is_multiple_of(2)
			&& matches!(self.fps, 15 | 30 | 60)
	}

	pub fn bit_rate(self) -> u32 {
		let pixels = u64::from(self.width) * u64::from(self.height);
		let base = if pixels <= 854 * 480 {
			2_000_000
		} else if pixels <= 1280 * 720 {
			4_000_000
		} else {
			(pixels.saturating_mul(8_000_000) / (1920 * 1080)).min(32_000_000)
		};
		(base * if self.fps == 60 { 2 } else { 1 }).min(50_000_000) as u32
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn source(width: u32, height: u32) -> Source {
		Source {
			id: SourceId::Display(1),
			name: "Synthetic display".into(),
			dimensions: Some((width, height)),
		}
	}

	#[test]
	fn ultrawide_presets_keep_source_shape_and_native_pixels() {
		assert_eq!(
			source(3440, 1440).output_dimensions(1080),
			Some((2580, 1080))
		);
		assert_eq!(source(3440, 1440).output_dimensions(0), Some((3440, 1440)));
		assert_eq!(source(5120, 1440).output_dimensions(0), Some((5120, 1440)));
		assert_eq!(
			source(5120, 1440).output_dimensions(1080),
			Some((3840, 1080))
		);
		assert_eq!(source(3441, 1441).output_dimensions(0), Some((3440, 1440)));
		assert_eq!(source(1280, 720).output_dimensions(2160), Some((1280, 720)));
		assert_eq!(
			source(7680, 4320).output_dimensions(2160),
			Some((3840, 2160))
		);
		assert!(source(7680, 4320).output_dimensions(0).is_none());
		assert!(source(0, 1440).output_dimensions(1080).is_none());
		assert_eq!(source(1080, 1920).output_dimensions(0), Some((1080, 1920)));
	}

	#[test]
	fn video_budget_and_bitrate_are_bounded() {
		let mut settings = Settings {
			source: SourceId::Display(1),
			width: 1920,
			height: 1080,
			fps: 60,
			cursor: true,
			audio: false,
		};
		assert_eq!(settings.bit_rate(), 16_000_000);
		settings.width = 3440;
		settings.height = 1440;
		assert!(settings.valid());
		assert_eq!(settings.bit_rate(), 38_222_222);
		settings.width = 5120;
		assert!(settings.valid());
		assert_eq!(settings.bit_rate(), 50_000_000);
		settings.width = 7680;
		assert!(!settings.valid());
		settings.width = 3441;
		assert!(!settings.valid());
		settings.width = 3440;
		settings.fps = 120;
		assert!(!settings.valid());
	}
}

#[derive(Debug)]
pub enum Event {
	Created {
		rtc_server: Id,
		rtc_channel: Id,
	},
	Server {
		token: Option<voice::Secret>,
		endpoint: Option<String>,
	},
	/// The stream is gone. `reason` names Discord's cause when it sent one we recognise.
	Deleted {
		reason: Option<&'static str>,
	},
	Failed(&'static str),
}
impl Event {
	pub(crate) fn bytes(&self) -> usize {
		match self {
			Self::Server { token, endpoint } => {
				token.as_ref().map_or(0, voice::Secret::bytes)
					+ endpoint.as_ref().map_or(0, String::capacity)
			}
			_ => 0,
		}
	}
}
