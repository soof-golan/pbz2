#[cfg(all(feature = "simd", target_arch = "aarch64", target_feature = "neon"))]
mod chosen {
    pub type Native = crate::Vectorized<fearless_simd::Neon>;

    #[inline]
    pub fn native() -> Native {
        match fearless_simd::Level::baseline().as_neon() {
            Some(token) => crate::Vectorized(token),
            None => unreachable!("the build enables neon"),
        }
    }
}

#[cfg(all(
    feature = "simd",
    any(target_arch = "x86", target_arch = "x86_64"),
    target_feature = "avx2",
    target_feature = "bmi1",
    target_feature = "bmi2",
    target_feature = "cmpxchg16b",
    target_feature = "f16c",
    target_feature = "fma",
    target_feature = "fxsr",
    target_feature = "lzcnt",
    target_feature = "movbe",
    target_feature = "popcnt",
    target_feature = "xsave"
))]
mod chosen {
    pub type Native = crate::Vectorized<fearless_simd::Avx2>;

    #[inline]
    pub fn native() -> Native {
        match fearless_simd::Level::baseline().as_avx2() {
            Some(token) => crate::Vectorized(token),
            None => unreachable!("the build enables avx2"),
        }
    }
}

#[cfg(all(
    feature = "simd",
    any(target_arch = "x86", target_arch = "x86_64"),
    target_feature = "fxsr",
    target_feature = "sse4.2",
    target_feature = "cmpxchg16b",
    target_feature = "popcnt",
    not(all(
        target_feature = "avx2",
        target_feature = "bmi1",
        target_feature = "bmi2",
        target_feature = "f16c",
        target_feature = "fma",
        target_feature = "lzcnt",
        target_feature = "movbe",
        target_feature = "xsave"
    ))
))]
mod chosen {
    pub type Native = crate::Vectorized<fearless_simd::Sse4_2>;

    #[inline]
    pub fn native() -> Native {
        match fearless_simd::Level::baseline().as_sse4_2() {
            Some(token) => crate::Vectorized(token),
            None => unreachable!("the build enables sse4.2"),
        }
    }
}

#[cfg(all(feature = "simd", target_arch = "wasm32", target_feature = "simd128"))]
mod chosen {
    pub type Native = crate::Vectorized<fearless_simd::WasmSimd128>;

    #[inline]
    pub fn native() -> Native {
        match fearless_simd::Level::baseline().as_wasm_simd128() {
            Some(token) => crate::Vectorized(token),
            None => unreachable!("the build enables simd128"),
        }
    }
}

#[cfg(not(all(
    feature = "simd",
    any(
        all(target_arch = "aarch64", target_feature = "neon"),
        all(
            any(target_arch = "x86", target_arch = "x86_64"),
            target_feature = "fxsr",
            target_feature = "sse4.2",
            target_feature = "cmpxchg16b",
            target_feature = "popcnt"
        ),
        all(target_arch = "wasm32", target_feature = "simd128")
    )
)))]
mod chosen {
    pub type Native = crate::Scalar;

    #[inline]
    pub const fn native() -> Native {
        crate::Scalar
    }
}

/// The [`crate::Backend`] this build runs, picked when it is compiled.
///
/// With the `simd` feature it is [`crate::Vectorized`] with the best SIMD instructions the
/// target enables: NEON on ARM, AVX2 or SSE4.2 on x86 (build with `-C target-cpu=native`
/// or a `target-feature` list to get them), and SIMD128 on WebAssembly. Otherwise it is
/// [`crate::Scalar`].
pub type Native = chosen::Native;

/// The [`Native`] backend.
#[inline]
#[must_use]
pub fn native() -> Native {
    chosen::native()
}
