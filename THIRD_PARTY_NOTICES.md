# Third-party notices

## Chiaki

This project is a derivative work of **Chiaki v2.2.0**, Copyright (C) the
Chiaki contributors, licensed under the GNU Affero General Public License,
version 3 only, with the OpenSSL linking exception (AGPL-3.0-only-OpenSSL).

Source: <https://git.sr.ht/~thestr4ng3r/chiaki>

The protocol key tables in `crates/chiaki-core/src/crypto/rpcrypt_keys.rs` and
the PSN OAuth client constants in `crates/server/src/psn.rs` originate from
Chiaki's public source. They were not extracted from a Sony SDK, firmware, or
decompiled Sony software by this project.

## AMD FidelityFX Super Resolution 1.0 (EASU and RCAS)

The WebGL2 sharpening pipeline in `crates/server/src/static/app.js` is a port
of the EASU (Edge Adaptive Spatial Upsampling) and RCAS (Robust Contrast
Adaptive Sharpening) shaders from AMD FidelityFX Super Resolution 1.0.

Source: <https://github.com/GPUOpen-Effects/FidelityFX-FSR>

```
FidelityFX Super Resolution Sample

Copyright (c) 2021 Advanced Micro Devices, Inc. All rights reserved.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files(the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and / or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions :

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```
