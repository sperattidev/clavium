use clavium_core::clav::{self, ClavError, SCHEMA_VERSION};
use clavium_core::midi::{MidiImportError, assign_unknown_hands_by_split, import_smf};
use clavium_core::validate::{IssueKind, Severity, is_publishable, validate};
use clavium_core::{Hand, Metadata, Note, PedalSpan, Piece, TempoChange, TempoMap, TimeSignature};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

const EPS: f64 = 1e-9;

fn ev(delta: u32, kind: TrackEventKind<'static>) -> TrackEvent<'static> {
    TrackEvent {
        delta: u28::new(delta),
        kind,
    }
}

fn on(delta: u32, key: u8, vel: u8) -> TrackEvent<'static> {
    ev(
        delta,
        TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOn {
                key: u7::new(key),
                vel: u7::new(vel),
            },
        },
    )
}

fn off(delta: u32, key: u8) -> TrackEvent<'static> {
    ev(
        delta,
        TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::NoteOff {
                key: u7::new(key),
                vel: u7::new(0),
            },
        },
    )
}

fn cc(delta: u32, controller: u8, value: u8) -> TrackEvent<'static> {
    ev(
        delta,
        TrackEventKind::Midi {
            channel: u4::new(0),
            message: MidiMessage::Controller {
                controller: u7::new(controller),
                value: u7::new(value),
            },
        },
    )
}

fn tempo(delta: u32, us_per_quarter: u32) -> TrackEvent<'static> {
    ev(
        delta,
        TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_quarter))),
    )
}

fn end() -> TrackEvent<'static> {
    ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack))
}

fn smf_bytes(format: Format, ppq: u16, tracks: Vec<Vec<TrackEvent<'static>>>) -> Vec<u8> {
    let smf = Smf {
        header: Header::new(format, Timing::Metrical(u15::new(ppq))),
        tracks,
    };
    let mut bytes = Vec::new();
    smf.write_std(&mut bytes).unwrap();
    bytes
}

fn note(pitch: u8, start_tick: u64, end_tick: u64) -> Note {
    Note {
        pitch,
        velocity: 80,
        start_tick,
        end_tick,
        hand: Hand::Right,
        finger: None,
    }
}

// --- Mapa de tempo ---

#[test]
fn tempo_map_sums_each_segment() {
    // 120 BPM durante 2 negras y luego 60 BPM.
    let map = TempoMap::new(
        480,
        vec![
            TempoChange {
                tick: 0,
                us_per_quarter: 500_000,
            },
            TempoChange {
                tick: 960,
                us_per_quarter: 1_000_000,
            },
        ],
    )
    .unwrap();
    assert!((map.tick_to_seconds(480) - 0.5).abs() < EPS);
    assert!((map.tick_to_seconds(960) - 1.0).abs() < EPS);
    assert!((map.tick_to_seconds(1440) - 2.0).abs() < EPS);
    assert!((map.seconds_to_tick(2.0) - 1440.0).abs() < 1e-6);
    assert!((map.bpm_at(1000) - 60.0).abs() < EPS);
}

#[test]
fn tempo_map_defaults_to_120_bpm_and_last_change_wins() {
    let map = TempoMap::new(
        96,
        vec![
            TempoChange {
                tick: 96,
                us_per_quarter: 400_000,
            },
            TempoChange {
                tick: 96,
                us_per_quarter: 250_000,
            },
        ],
    )
    .unwrap();
    assert_eq!(map.changes().len(), 2);
    assert_eq!(map.us_per_quarter_at(0), 500_000);
    assert_eq!(map.us_per_quarter_at(96), 250_000);
    assert!((map.tick_to_seconds(192) - 0.75).abs() < EPS);
}

#[test]
fn tempo_map_has_no_drift_over_many_changes() {
    // 10 000 cambios alternando tempos con fracciones de microsegundo por tick.
    let changes = (0..10_000u64)
        .map(|i| TempoChange {
            tick: i * 7,
            us_per_quarter: if i % 2 == 0 { 333_333 } else { 571_429 },
        })
        .collect();
    let map = TempoMap::new(480, changes).unwrap();
    let exact_us = 5_000.0 * 7.0 * (333_333.0 + 571_429.0) / 480.0;
    assert!((map.tick_to_seconds(70_000) - exact_us / 1e6).abs() < 1e-9);
}

#[test]
fn tempo_map_rejects_zero_values() {
    assert!(TempoMap::constant(0, 500_000).is_err());
    assert!(TempoMap::constant(480, 0).is_err());
}

// --- Importación MIDI ---

#[test]
fn import_handles_running_status_and_velocity_zero() {
    // Escrito a mano para controlar el running status y las longitudes variables.
    #[rustfmt::skip]
    let bytes: Vec<u8> = vec![
        b'M', b'T', b'h', b'd', 0, 0, 0, 6, 0, 0, 0, 1, 0, 96,
        b'M', b'T', b'r', b'k', 0, 0, 0, 18,
        0x00, 0x90, 60, 100,  // Do4 suena
        0x60, 60, 0,          // +96 ticks, running status, velocity 0 = Note Off
        0x00, 64, 90,         // Mi4 suena
        0x81, 0x40, 64, 0,    // +192 ticks (longitud variable de 2 bytes), Mi4 se suelta
        0x00, 0xFF, 0x2F, 0x00,
    ];
    let piece = import_smf(&bytes).unwrap();
    assert_eq!(piece.notes.len(), 2);

    let c4 = piece.notes[0];
    assert_eq!(
        (c4.pitch, c4.velocity, c4.start_tick, c4.end_tick),
        (60, 100, 0, 96)
    );
    let e4 = piece.notes[1];
    assert_eq!((e4.pitch, e4.start_tick, e4.end_tick), (64, 96, 288));

    // 120 BPM por defecto: una negra = 0,5 s.
    let (start, stop) = piece.note_seconds(&e4);
    assert!((start - 0.5).abs() < EPS);
    assert!((stop - 1.5).abs() < EPS);
}

#[test]
fn import_reads_tempo_changes_from_the_conductor_track() {
    let bytes = smf_bytes(
        Format::Parallel,
        480,
        vec![
            vec![tempo(0, 500_000), tempo(960, 1_000_000), end()],
            vec![on(1440, 72, 90), off(480, 72), end()],
        ],
    );
    let piece = import_smf(&bytes).unwrap();
    let (start, stop) = piece.note_seconds(&piece.notes[0]);
    assert!((start - 2.0).abs() < EPS);
    assert!((stop - 3.0).abs() < EPS);
}

#[test]
fn import_assigns_hands_by_track_pitch() {
    // La primera pista es la grave: igual tiene que quedar como mano izquierda.
    let bytes = smf_bytes(
        Format::Parallel,
        480,
        vec![
            vec![on(0, 48, 70), off(480, 48), end()],
            vec![
                on(0, 76, 70),
                off(480, 76),
                on(0, 79, 70),
                off(480, 79),
                end(),
            ],
        ],
    );
    let piece = import_smf(&bytes).unwrap();
    for n in &piece.notes {
        let expected = if n.pitch < 60 {
            Hand::Left
        } else {
            Hand::Right
        };
        assert_eq!(n.hand, expected, "nota {}", n.pitch);
    }
}

#[test]
fn import_splits_single_track_at_middle_c() {
    let bytes = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            on(0, 59, 70),
            on(0, 60, 70),
            off(480, 59),
            off(0, 60),
            end(),
        ]],
    );
    let mut piece = import_smf(&bytes).unwrap();
    assert_eq!(piece.notes[0].hand, Hand::Left);
    assert_eq!(piece.notes[1].hand, Hand::Right);

    piece.notes[0].hand = Hand::Unknown;
    assign_unknown_hands_by_split(&mut piece, 55);
    assert_eq!(piece.notes[0].hand, Hand::Right);
}

#[test]
fn import_reconstructs_sustain_pedal() {
    let bytes = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            cc(0, 64, 127),
            on(0, 60, 80),
            off(240, 60),
            cc(720, 64, 0),
            end(),
        ]],
    );
    let piece = import_smf(&bytes).unwrap();
    assert_eq!(
        piece.sustain,
        vec![PedalSpan {
            start_tick: 0,
            end_tick: 960
        }]
    );
    let n = piece.notes[0];
    assert_eq!(n.end_tick, 240);
    assert_eq!(piece.sounding_end_tick(&n), 960);
}

#[test]
fn import_closes_retriggered_and_unreleased_notes() {
    let bytes = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![
            on(0, 60, 80),
            on(480, 60, 90),
            ev(480, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
        ]],
    );
    let piece = import_smf(&bytes).unwrap();
    let spans: Vec<_> = piece
        .notes
        .iter()
        .map(|n| (n.start_tick, n.end_tick))
        .collect();
    assert_eq!(spans, vec![(0, 480), (480, 960)]);
}

#[test]
fn import_rejects_format_2() {
    let bytes = smf_bytes(Format::Sequential, 480, vec![vec![end()]]);
    assert!(matches!(
        import_smf(&bytes),
        Err(MidiImportError::UnsupportedFormat)
    ));
}

#[test]
fn import_records_unverified_provenance() {
    let bytes = smf_bytes(
        Format::SingleTrack,
        480,
        vec![vec![on(0, 60, 80), off(480, 60), end()]],
    );
    let provenance = import_smf(&bytes).unwrap().provenance.unwrap();
    assert_eq!(provenance.confidence, clavium_core::Confidence::D);
    assert_eq!(provenance.file_sha256.unwrap().len(), 64);
}

// --- Compases ---

#[test]
fn bar_ticks_follow_time_signature_changes() {
    let mut piece = Piece::new(
        Metadata::titled("Compases"),
        TempoMap::constant(480, 500_000).unwrap(),
    );
    piece.time_signatures = vec![
        TimeSignature {
            tick: 0,
            numerator: 4,
            denominator: 4,
        },
        TimeSignature {
            tick: 3840,
            numerator: 3,
            denominator: 4,
        },
    ];
    piece.notes = vec![note(60, 5900, 6000)];
    assert_eq!(piece.bar_ticks(), vec![0, 1920, 3840, 5280]);
}

// --- Validación ---

#[test]
fn validation_flags_broken_notes() {
    let mut piece = Piece::new(
        Metadata::titled("Rota"),
        TempoMap::constant(480, 500_000).unwrap(),
    );
    piece.notes = vec![
        note(10, 0, 480), // fuera del rango del piano
        note(60, 0, 480),
        note(60, 240, 720), // se superpone con la anterior
        note(64, 480, 480), // dura 0
    ];
    piece.notes[1].hand = Hand::Unknown;

    let issues = validate(&piece);
    let kinds: Vec<_> = issues.iter().map(|i| i.kind.clone()).collect();
    assert!(kinds.contains(&IssueKind::OutOfRange { pitch: 10 }));
    assert!(kinds.contains(&IssueKind::Overlap { pitch: 60 }));
    assert!(kinds.contains(&IssueKind::ZeroDuration { pitch: 64 }));
    assert!(kinds.contains(&IssueKind::UnknownHand { pitch: 60 }));
    assert!(!is_publishable(&issues));
}

#[test]
fn validation_accepts_a_clean_piece_and_warns_on_extreme_tempo() {
    let mut piece = Piece::new(
        Metadata::titled("Limpia"),
        TempoMap::constant(480, 100_000).unwrap(),
    );
    piece.notes = vec![note(60, 0, 480), note(64, 480, 960)];
    let issues = validate(&piece);
    assert_eq!(issues.len(), 1);
    assert_eq!(issues[0].severity, Severity::Warning); // 600 BPM
    assert!(is_publishable(&issues));
}

// --- Formato .clav ---

#[test]
fn clav_round_trip_preserves_the_piece() {
    let bytes = smf_bytes(
        Format::Parallel,
        480,
        vec![
            vec![tempo(0, 600_000), tempo(960, 450_000), end()],
            vec![
                cc(0, 64, 127),
                on(0, 72, 90),
                off(480, 72),
                cc(480, 64, 0),
                end(),
            ],
            vec![on(0, 48, 60), off(960, 48), end()],
        ],
    );
    let piece = import_smf(&bytes).unwrap();

    let mut file = Vec::new();
    clav::write(&piece, &mut file).unwrap();
    assert_eq!(&file[..2], &[0x1f, 0x8b], "tiene que ser gzip");

    let loaded = clav::read(file.as_slice()).unwrap();
    assert_eq!(loaded, piece);
    assert!((loaded.tempo.tick_to_seconds(1440) - piece.tempo.tick_to_seconds(1440)).abs() < EPS);
}

#[test]
fn clav_rejects_newer_schema_versions() {
    let json = format!(
        r#"{{"schema_version":{},"piece":{{}}}}"#,
        SCHEMA_VERSION + 1
    );
    let mut file = Vec::new();
    let mut encoder = flate2::write::GzEncoder::new(&mut file, flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, json.as_bytes()).unwrap();
    encoder.finish().unwrap();

    assert!(matches!(
        clav::read(file.as_slice()),
        Err(ClavError::UnsupportedVersion { .. })
    ));
}

// --- Biblioteca inicial ---

#[test]
fn bundled_library_pieces_parse_and_validate() {
    // Cada archivo de library/ se valida solo: sumar una pieza no requiere tocar el test.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../library");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|x| x != "txt") || path.ends_with("rutas.txt") {
            continue;
        }
        count += 1;
        let text = std::fs::read_to_string(&path).unwrap();
        let text = text.as_str();
        let piece = clavium_core::notation::parse_piece(text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let issues = validate(&piece);
        assert!(issues.is_empty(), "{}: {issues:?}", piece.metadata.title);
        let last_section = piece.sections.last().unwrap();
        assert_eq!(
            last_section.end_tick,
            piece.end_tick(),
            "{}",
            piece.metadata.title
        );
    }
    assert!(count >= 3, "se esperaban al menos 3 piezas en library/");
}
