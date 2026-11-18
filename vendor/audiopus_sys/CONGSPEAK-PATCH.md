# Local build patch

Upstream: audiopus_sys 0.2.2 (ISC license, original source and license retained).

Only build.rs differs: set CMake policy minimum 3.5 for CMake 4, explicitly select static CRT, and disable the old SSE4.1 implementation only when cross-compiling MSVC from a non-Windows host. This preserves the x64/SSE2 baseline instead of enabling SSE4.1 for every Opus source file. Native Windows MSVC builds retain the original SIMD selection.
