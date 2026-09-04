//! tanh soft-clip saturator, unity gain at full scale.

use crate::{InsertFx, ParamId};

pub struct Saturate {
    drive: f32,
    norm: f32,
}

impl Saturate {
    pub fn new(drive: f32) -> Self {
        let drive = drive.clamp(0.1, 8.0);
        Saturate { drive, norm: drive.tanh().max(1e-6) }
    }
}

impl InsertFx for Saturate {
    fn render(&mut self, io: &mut [f32]) {
        for s in io.iter_mut() {
            *s = (*s * self.drive).tanh() / self.norm;
        }
    }

    fn set_param(&mut self, param: ParamId, value: f32) {
        use crate::params::SATURATE_DRIVE;
        if param == SATURATE_DRIVE {
            let drive = value.clamp(0.1, 8.0);
            self.norm = drive.tanh().max(1e-6);
            self.drive = drive;
        }
    }

    fn reset(&mut self) {}
}
