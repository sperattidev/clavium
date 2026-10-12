# Clavium

Aprendé piano con tu teclado MIDI. Conectás el teclado, elegís una canción de la
biblioteca de Clavium y tocás mientras las notas caen sobre un teclado en
pantalla; la app compara cada nota con la partitura y te dice si acertaste, si
fallaste o si llegaste tarde.

App de escritorio para Windows y macOS, escrita en Rust.

## Estado

Fase 0 (fundamentos): modelo de pieza, mapa de tempo, importación MIDI,
validación y formato `.clav`. Ver el roadmap en la documentación del producto.

## Estructura

```text
crates/
  clavium-core/   modelo de pieza, mapa de tempo, importación MIDI, formato .clav
```

Los próximos crates (`clavium-midi`, `clavium-eval`, `clavium-audio`,
`clavium-render`, `clavium-app`) se suman en las fases siguientes.

## Desarrollo

Requiere [Rust](https://rustup.rs) estable.

```bash
cargo test --all
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
