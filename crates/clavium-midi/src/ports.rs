/// Qué tan probable es que un puerto MIDI sea un teclado físico.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum PortKind {
    /// Un dispositivo conectado (USB o Bluetooth).
    Device,
    /// Un puerto virtual o de software (loopMIDI, bus IAC, Midi Through).
    Virtual,
}

const VIRTUAL_HINTS: &[&str] = &[
    "through",
    "iac",
    "loopmidi",
    "loopbe",
    "virtual",
    "rtpmidi",
    "network",
    "wavetable",
];

pub fn classify_port(name: &str) -> PortKind {
    let lower = name.to_lowercase();
    if VIRTUAL_HINTS.iter().any(|hint| lower.contains(hint)) {
        PortKind::Virtual
    } else {
        PortKind::Device
    }
}

/// Elige el puerto al que conectarse: el preferido si está disponible; si no,
/// el primer dispositivo físico; si no, el primer puerto virtual. Devuelve
/// el índice en `names`. Los puertos de `skip` (por ejemplo, ocupados por
/// otra app) no se eligen.
pub fn choose_port(names: &[String], preferred: Option<&str>, skip: &[String]) -> Option<usize> {
    let candidates = || names.iter().enumerate().filter(|(_, n)| !skip.contains(n));
    if let Some(preferred) = preferred
        && let Some((i, _)) = candidates().find(|(_, n)| n.as_str() == preferred)
    {
        return Some(i);
    }
    candidates()
        .min_by_key(|(i, n)| (classify_port(n), *i))
        .map(|(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn prefers_physical_devices_over_virtual_ports() {
        let ports = names(&["loopMIDI Port", "Midi Through Port-0", "Digital Piano"]);
        assert_eq!(choose_port(&ports, None, &[]), Some(2));
    }

    #[test]
    fn remembers_the_preferred_keyboard() {
        let ports = names(&["Arturia KeyLab", "Yamaha P-125"]);
        assert_eq!(choose_port(&ports, Some("Yamaha P-125"), &[]), Some(1));
        assert_eq!(choose_port(&ports, Some("Casio CDP"), &[]), Some(0));
    }

    #[test]
    fn falls_back_to_virtual_ports_and_skips_busy_ones() {
        let ports = names(&["IAC Driver Bus 1", "Roland FP-30"]);
        assert_eq!(
            choose_port(&ports, None, &names(&["Roland FP-30"])),
            Some(0)
        );
        assert_eq!(choose_port(&[], None, &[]), None);
    }
}
