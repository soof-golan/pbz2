use pbz2_core::{Backend, Native, native};

#[cfg(all(
    feature = "simd",
    target_arch = "x86_64",
    not(target_feature = "sse4.2")
))]
type Sse4_2 = pbz2_core::Vectorized<pbz2_core::fearless_simd::Sse4_2>;

pub(crate) trait Engine: Backend + Send + Sync + 'static {}

impl<B: Backend + Send + Sync + 'static> Engine for B {}

pub(crate) trait Pipeline {
    type Running<B: Engine>;
}

pub(crate) trait Starts<P: Pipeline> {
    fn start<B: Engine>(self, backend: B) -> P::Running<B>;
}

pub(crate) enum Started<P: Pipeline> {
    Built(P::Running<Native>),
    #[cfg(all(
        feature = "simd",
        target_arch = "x86_64",
        not(target_feature = "sse4.2")
    ))]
    Sse4_2(P::Running<Sse4_2>),
}

pub(crate) fn start<P: Pipeline>(work: impl Starts<P>) -> Started<P> {
    #[cfg(all(
        feature = "simd",
        target_arch = "x86_64",
        not(target_feature = "sse4.2")
    ))]
    if let Some(token) = pbz2_core::fearless_simd::Level::new().as_sse4_2() {
        return Started::Sse4_2(work.start(pbz2_core::Vectorized(token)));
    }
    Started::Built(work.start(native()))
}

macro_rules! on_engine {
    ($started:expr, $running:ident => $body:expr) => {
        match $started {
            $crate::engine::Started::Built($running) => $body,
            #[cfg(all(
                feature = "simd",
                target_arch = "x86_64",
                not(target_feature = "sse4.2")
            ))]
            $crate::engine::Started::Sse4_2($running) => $body,
        }
    };
}

pub(crate) use on_engine;
