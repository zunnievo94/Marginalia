# Third-party software in Marginalia

Marginalia is licensed under the GNU General Public License, version 3 or
(at your option) any later version (`LICENSE`). The complete source code
for each release is available at <https://github.com/zunnievo94/Marginalia>
under the matching `vX.Y.Z` tag.

The released programs (desktop app and `marginalia` command) contain the
components below; all of their licenses are compatible with the GPL-3.0.

| Component | Use | License |
| --- | --- | --- |
| [eSpeak NG](https://github.com/espeak-ng/espeak-ng) | phonemizer for voices (via sherpa-onnx) | GPL-3.0-or-later |
| [epub](https://github.com/danigm/epub-rs) | EPUB parsing | GPL-3.0 |
| [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) | speech synthesis runtime | Apache-2.0 |
| [ONNX Runtime](https://github.com/microsoft/onnxruntime) | neural network inference | MIT |
| [piper-phonemize](https://github.com/rhasspy/piper-phonemize) | phonemizer (via sherpa-onnx) | MIT |
| kaldi-native-fbank, kaldi-decoder, kaldifst, OpenFst, SentencePiece | parts of sherpa-onnx | Apache-2.0 |
| KISS FFT | part of sherpa-onnx | BSD-3-Clause |
| [Tauri](https://tauri.app) | desktop app framework | MIT OR Apache-2.0 |
| [rodio](https://github.com/RustAudio/rodio) / cpal | audio output | MIT OR Apache-2.0 |
| cssparser, selectors (via Tauri) | HTML/CSS handling | MPL-2.0 |
| Other Rust crates | see `Cargo.lock` | MIT, Apache-2.0, BSD, ISC, Zlib, Unicode-3.0, CDLA-Permissive-2.0 (permissive) |

## Voice models

Voice models are **not** included; the app downloads them when you ask.
Each keeps its own license, which applies to your use of it:

- Piper voices: listed in each model's `MODEL_CARD` (most are CC BY 4.0,
  public domain or MIT; a few datasets restrict commercial use).
- Kokoro: Apache-2.0.
- Supertonic 3: MIT (code) and OpenRAIL-M (weights), which forbids certain
  harmful uses.
