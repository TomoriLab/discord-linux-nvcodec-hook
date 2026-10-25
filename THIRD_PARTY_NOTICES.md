# Third-party notices

The project's [MIT license](LICENSE) covers original project code. The three bundled API headers retain their own notices. This file records their provenance and reproduces their shared permission and warranty text for distributions that omit the header files.

## Bundled NVIDIA codec API headers

All three files were copied without modification from [FFmpeg's `nv-codec-headers` repository](https://github.com/FFmpeg/nv-codec-headers) at commit [`f2053071ea93137255b6dcf42e9b4c9e0965a44e`](https://github.com/FFmpeg/nv-codec-headers/commit/f2053071ea93137255b6dcf42e9b4c9e0965a44e). The upstream [README](https://github.com/FFmpeg/nv-codec-headers/blob/f2053071ea93137255b6dcf42e9b4c9e0965a44e/README) identifies the corresponding NVIDIA Video Codec SDK version as 13.1.15.

| Bundled file | Upstream source | Copyright notice in the file |
| --- | --- | --- |
| `include/ffnvcodec/dynlink_cuda.h` | [`dynlink_cuda.h`](https://github.com/FFmpeg/nv-codec-headers/blob/f2053071ea93137255b6dcf42e9b4c9e0965a44e/include/ffnvcodec/dynlink_cuda.h) | Copyright (c) 2016 |
| `include/ffnvcodec/dynlink_cuviddec.h` | [`dynlink_cuviddec.h`](https://github.com/FFmpeg/nv-codec-headers/blob/f2053071ea93137255b6dcf42e9b4c9e0965a44e/include/ffnvcodec/dynlink_cuviddec.h) | Copyright (c) 2010-2026 NVIDIA Corporation |
| `include/ffnvcodec/dynlink_nvcuvid.h` | [`dynlink_nvcuvid.h`](https://github.com/FFmpeg/nv-codec-headers/blob/f2053071ea93137255b6dcf42e9b4c9e0965a44e/include/ffnvcodec/dynlink_nvcuvid.h) | Copyright (c) 2010-2026 NVIDIA Corporation |

Each header states that its copyright notice applies to that header file only. Each also contains the following identical permission and warranty text:

> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the software, and to permit persons to whom the software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

## Generated bindings and distributions

`src/ffi.rs` was generated with bindgen 0.72.1 from the three bundled headers through `wrapper.h`. Its header points here for the source notices. Regenerating it with `scripts/generate-bindings.sh` preserves that pointer. No CUDA Toolkit header is used to generate the committed bindings.

Keep this file and the project `LICENSE` with source releases. If distributing a compiled library without the header files, include this file alongside the library so the upstream copyright, permission, and warranty notices remain available. The build and installer scripts do not create a redistributable release archive automatically.

Discord binaries, NVIDIA driver libraries, FFmpeg, libx265, and the bindgen executable are not included in this repository. The HEVC test fixtures are locally generated patterns; they do not contain third-party video footage. Names and trademarks remain with their respective owners.
