# Third-party software

The MIT license covers this project's original Rust implementation. It does not relicense Laitis, the speech model, native Vosk runtime, or Rust dependencies.

- Vosk API 0.3.45: [upstream source and license](https://github.com/alphacep/vosk-api/tree/v0.3.45), Apache-2.0. Windows binaries are the unmodified DLLs from the [official PyPI wheel](https://pypi.org/project/vosk/0.3.45/).
- Russian model `vosk-model-small-ru-0.22`: Apache-2.0, [official model catalog](https://alphacephei.com/vosk/models). Model files and their original README are preserved in the portable bundle.
- The official wheel includes GCC `libgcc_s_seh-1.dll`, `libstdc++-6.dll` and MinGW `libwinpthread-1.dll`. These retain their upstream licenses: [GCC licensing](https://gcc.gnu.org/onlinedocs/libstdc++/manual/license.html), [GCC runtime exception](https://www.gnu.org/licenses/gcc-exception-3.1.html), [MinGW-w64 licensing](https://github.com/mingw-w64/mingw-w64/tree/master/COPYING). The project does not modify or incorporate these runtimes into its Rust source.
- Rust dependency versions and transitive dependencies are locked in `Cargo.lock`; their licenses remain those of their respective upstream projects.

`Laitis.exe`, its embedded assemblies, authentication constants and extracted source are not distributed. The reverse-engineering report describes observed interfaces and behavior; the new implementation is written independently.
