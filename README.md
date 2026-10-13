# Clavium

Aprendé piano con tu teclado MIDI. Conectás el teclado, elegís una canción de la
biblioteca de Clavium y tocás mientras las notas caen sobre un teclado en
pantalla; la app compara cada nota con la partitura y te dice si acertaste, si
fallaste o si llegaste tarde.

App de escritorio para Windows y macOS, escrita en Rust.

## Estado

Fase 1 (teclado en vivo): la app detecta el teclado MIDI, se reconecta sola e
ilumina en pantalla las teclas que tocás. La fase 0 dejó listos el modelo de
pieza, el mapa de tempo, la importación MIDI, la validación y el formato
`.clav`. Ver el roadmap en la documentación del producto.

## Estructura

```text
crates/
  clavium-core/   modelo de pieza, mapa de tempo, importación MIDI, formato .clav
  clavium-midi/   detección del teclado, reconexión y entrada en vivo
  clavium-audio/  piano interno y salida de audio
  clavium-app/    la app: teclado en pantalla que se ilumina y suena con lo que tocás
```

Los próximos crates (`clavium-eval`, `clavium-render`) se suman en las fases
siguientes.

## Desarrollo

Requiere [Rust](https://rustup.rs) estable. En Windows, con el toolchain MSVC
(Visual Studio Build Tools con "Desarrollo para el escritorio con C++").

```bash
cargo test --all
cargo run -p clavium-app
```

Para ver cómo queda un MIDI dentro de Clavium y guardarlo como `.clav`:

```bash
cargo run -p clavium-core --example inspect -- pieza.mid pieza.clav
```

## Licencia

El código está bajo la [Mozilla Public License 2.0](LICENSE). Las
contribuciones externas requieren aceptar un acuerdo de contribución (CLA),
que se publicará antes de abrir el proyecto a colaboradores. El nombre y el
logo de Clavium no están cubiertos por la licencia del código.
