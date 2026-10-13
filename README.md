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
  clavium-eval/   motor de evaluación: aciertos, errores, modos de práctica y puntaje
  clavium-audio/  piano interno y salida de audio
  clavium-catalog/ arma el paquete de obras completas desde el Mutopia Project
  clavium-app/    la app: biblioteca, notas que caen, evaluación y resultados
library/         piezas de la biblioteca inicial
```

La biblioteca inicial está en `library/`, escrita en un formato de texto que
lee `clavium-core::notation`.

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

## Biblioteca ampliada

Las obras completas se descargan aparte de la app. El paquete se arma con
`clavium-catalog` a partir del [Mutopia Project](https://www.mutopiaproject.org):
solo obras para teclado solo con licencia de dominio público o Creative
Commons Attribution (las ShareAlike quedan afuera). Cada obra lleva su ficha de
procedencia y entra como "sin revisar" hasta que alguien la toca completa.

```bash
git clone --depth 1 https://github.com/MutopiaProject/MutopiaProject.git
```

```bash
cargo run -p clavium-catalog --release -- mutopia MutopiaProject catalogo --zip clavium-catalog.zip
```

## Sonido

Clavium suena con un piano sintetizado propio, sin descargas. Desde la app se
puede descargar con un clic un piano de samples real, que se instala en la
carpeta de datos del usuario:

- **Yamaha C5 (Salamander Grand Piano V3)**, por Alexander Holm, con la
  afinación "Retuned" de Markus Fiedler y la versión SFZ de
  [sfzinstruments](https://github.com/sfzinstruments/SalamanderGrandPiano)
  (kinwie). Licencia Creative Commons Attribution 3.0.

`clavium-audio` incluye un lector de instrumentos SFZ y un sampler, así que
se pueden sumar otras librerías de samples con licencia libre.

```bash
cargo run -p clavium-app --release -- --instalar-piano estandar
```

## Publicar una versión

Al subir una etiqueta `v*` (por ejemplo `v0.1.0`), el flujo `Versión` arma
`Clavium-windows-x64.zip` y `Clavium-macos-arm64.zip` y crea el release. Se
puede correr a mano desde Actions para probar sin publicar. La página de
descarga está en `docs/` para GitHub Pages. Los créditos están en
[CREDITOS.md](CREDITOS.md).

Pendiente para distribuir sin advertencias: firma de código en Windows y firma
y notarización en macOS (requieren certificados del desarrollador).
