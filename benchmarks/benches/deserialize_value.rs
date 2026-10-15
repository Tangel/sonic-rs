#[macro_use]
extern crate criterion;

use std::{fs::File, hint::black_box, io::Read, str::from_utf8_unchecked};

use criterion::{BatchSize, Criterion, SamplingMode, Throughput, criterion_group};

include!("./common.rs");

#[cfg(not(target_env = "msvc"))]
#[global_allocator]
static GLOBAL: jemallocator::Jemalloc = jemallocator::Jemalloc;

// Materialize the DOM before timed drop; black-boxing only () cannot protect it.
fn simdjson_to_borrowed_value(data: &mut [u8]) {
    black_box(simd_json::to_borrowed_value(black_box(data)).unwrap());
}

fn simdjson_to_owned_value(data: &mut [u8]) {
    black_box(simd_json::to_owned_value(black_box(data)).unwrap());
}

fn serde_from_slice(data: &[u8]) {
    let value: serde_json::Value = serde_json::from_slice(black_box(data)).unwrap();
    black_box(value);
}

fn serde_from_str(data: &[u8]) {
    let data = unsafe { from_utf8_unchecked(data) };
    let value: serde_json::Value = serde_json::from_str(black_box(data)).unwrap();
    black_box(value);
}

fn sonic_rs_from_slice(data: &[u8], cfg: SonicConfig) {
    black_box(do_sonic_rs_from_slice(black_box(data), cfg).unwrap());
}

fn sonic_rs_from_slice_unchecked(data: &[u8]) {
    let value: sonic_rs::Value =
        unsafe { sonic_rs::from_slice_unchecked(black_box(data)).unwrap() };
    black_box(value);
}

// fn sonic_rs_skip_one(data: &[u8]) {
//     unsafe {
//         let data = from_utf8_unchecked(data);
//         let empty: &[&str] = &[];
//         let _ = sonic_rs::get_unchecked(data, empty).unwrap();
//     }
// }

fn sonic_rs_to_serdejson_value(data: &[u8]) {
    let value: serde_json::Value = sonic_rs::from_slice(black_box(data)).unwrap();
    black_box(value);
}

fn sonic_rs_to_simdjson_value(data: &[u8]) {
    let value: simd_json::OwnedValue = sonic_rs::from_slice(black_box(data)).unwrap();
    black_box(value);
}

macro_rules! bench_file {
    ($name:ident) => {
        #[allow(unused)]
        fn $name(c: &mut Criterion) {
            let core_ids = core_affinity::get_core_ids().unwrap();
            core_affinity::set_for_current(core_ids[0]);

            let mut vec = Vec::new();
            let root = env!("CARGO_MANIFEST_DIR").to_owned();
            File::open(root + concat!("/benches/testdata/", stringify!($name), ".json"))
                .unwrap()
                .read_to_end(&mut vec)
                .unwrap();

            // verify sonic-rs parse
            let serde_out: serde_json::Value = serde_json::from_slice(&vec).unwrap();

            let value: sonic_rs::Value = sonic_rs::from_slice(&vec).unwrap();
            let out = sonic_rs::to_string(&value).unwrap();
            let rs_out1: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(rs_out1, serde_out);

            let mut group = c.benchmark_group(stringify!($name));
            group.sampling_mode(SamplingMode::Flat);

            group.bench_with_input("sonic_rs_dom::from_slice", &vec, |b, data| {
                b.iter_batched(
                    || data,
                    |bytes| sonic_rs_from_slice(&bytes, SONIC_DEFAULT_CFG),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input("sonic_rs_dom::from_slice_use_rawnum", &vec, |b, data| {
                b.iter_batched(
                    || data,
                    |bytes| sonic_rs_from_slice(&bytes, SONIC_USE_RAWNUM_CFG),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input("sonic_rs_dom::from_slice_unchecked", &vec, |b, data| {
                b.iter_batched(
                    || data,
                    |bytes| sonic_rs_from_slice_unchecked(&bytes),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input(
                "sonic_rs_to_serde_json_value::from_slice_unchecked",
                &vec,
                |b, data| {
                    b.iter_batched(
                        || data,
                        |bytes| sonic_rs_to_serdejson_value(&bytes),
                        BatchSize::SmallInput,
                    )
                },
            );

            group.bench_with_input(
                "sonic_rs_to_simd_json_value::from_slice_unchecked",
                &vec,
                |b, data| {
                    b.iter_batched(
                        || data,
                        |bytes| sonic_rs_to_simdjson_value(&bytes),
                        BatchSize::SmallInput,
                    )
                },
            );

            // group.bench_with_input("sonic_rs::skip_one", &vec, |b, data| {
            //     b.iter_batched(
            //         || data,
            //         |bytes| sonic_rs_skip_one(&bytes),
            //         BatchSize::SmallInput,
            //     )
            // });

            // group.bench_with_input("sonic_rs::to_serdejson_value", &vec, |b, data| {
            //     b.iter_batched(
            //         || data,
            //         |bytes| sonic_rs_to_serdejson_value(&bytes),
            //         BatchSize::SmallInput,
            //     )
            // });

            group.bench_with_input("serde_json::from_slice", &vec, |b, data| {
                b.iter_batched(
                    || data,
                    |bytes| serde_from_slice(&bytes),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input("serde_json::from_str", &vec, |b, data| {
                b.iter_batched(
                    || data,
                    |bytes| serde_from_str(&bytes),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input("simd_json::slice_to_owned_value", &vec, |b, data| {
                b.iter_batched(
                    || data.clone(),
                    |mut bytes| simdjson_to_owned_value(&mut bytes),
                    BatchSize::SmallInput,
                )
            });

            group.bench_with_input("simd_json::slice_to_borrowed_value", &vec, |b, data| {
                b.iter_batched(
                    || data.clone(),
                    |mut bytes| simdjson_to_borrowed_value(&mut bytes),
                    BatchSize::SmallInput,
                )
            });
            group.throughput(Throughput::Bytes(vec.len() as u64));
        }
    };
}

bench_file!(book);
bench_file!(canada);
bench_file!(citm_catalog);
bench_file!(twitter);
bench_file!(github_events);
bench_file!(golang_source);
bench_file!(lottie);

criterion_group!(
    benches,
    canada,
    citm_catalog,
    twitter,
    github_events,
    book,
    golang_source,
    lottie,
);
criterion_main!(benches);
