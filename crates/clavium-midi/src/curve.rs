/// Curva de sensibilidad: cómo se traduce la fuerza con la que se toca en
/// la velocity que usa Clavium. Algunos controladores mandan valores muy
/// bajos aunque se toque normal; la curva liviana los compensa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VelocityCurve {
    /// Con poca fuerza ya suena fuerte.
    Light,
    #[default]
    Normal,
    /// Hace falta más fuerza para sonar fuerte.
    Heavy,
    /// Todas las notas suenan igual, sin importar la fuerza.
    Fixed,
}

impl VelocityCurve {
    pub const ALL: [VelocityCurve; 4] = [
        VelocityCurve::Light,
        VelocityCurve::Normal,
        VelocityCurve::Heavy,
        VelocityCurve::Fixed,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VelocityCurve::Light => "Liviana",
            VelocityCurve::Normal => "Normal",
            VelocityCurve::Heavy => "Pesada",
            VelocityCurve::Fixed => "Fija",
        }
    }

    /// Aplica la curva. Nunca devuelve 0, que en MIDI significa soltar la tecla.
    pub fn apply(self, velocity: u8) -> u8 {
        let v = f32::from(velocity.min(127)) / 127.0;
        let out = match self {
            VelocityCurve::Light => v.powf(0.5),
            VelocityCurve::Normal => v,
            VelocityCurve::Heavy => v.powf(1.6),
            VelocityCurve::Fixed => 100.0 / 127.0,
        };
        ((out * 127.0).round() as u8).clamp(1, 127)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_curve_is_identity() {
        for v in 1..=127 {
            assert_eq!(VelocityCurve::Normal.apply(v), v);
        }
    }

    #[test]
    fn light_curve_lifts_soft_notes() {
        assert!(VelocityCurve::Light.apply(10) >= 35);
        assert_eq!(VelocityCurve::Light.apply(127), 127);
        assert!(VelocityCurve::Heavy.apply(64) < 64);
    }

    #[test]
    fn curves_never_return_zero_and_are_monotonic() {
        for curve in VelocityCurve::ALL {
            let mut prev = 0;
            for v in 1..=127 {
                let out = curve.apply(v);
                assert!(out >= 1 && out >= prev);
                prev = out;
            }
        }
    }
}
