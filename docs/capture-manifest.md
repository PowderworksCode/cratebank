# Capture manifest

The capture contract has two required measurement sources: stable Cargo
`--timings` and samply around the same build.

## Identity and configuration

| field | source |
| --- | --- |
| package name and version | Cargo timing unit |
| target and compilation mode | Cargo timing unit |
| resolved feature set | Cargo timing unit and rustc arguments |
| dependency-unblocking edges | Cargo timing unit |
| opt-level, debuginfo, codegen units, panic, LTO, incremental, target features | scrubbed rustc arguments in samply |
| compiler version and host | Cargo timing report summary |
| profile and jobs | Cargo timing report summary |
| CI presence | client process |

The client never sends source paths or full command lines. Path values are
removed; program paths such as a linker or wrapper are reduced to a basename.

## Unit measurements

| field | source | quantity |
| --- | --- | --- |
| unit start and duration | Cargo timing unit | wall clock |
| frontend section | Cargo timing section | wall clock |
| codegen section | Cargo timing section | wall clock |
| link section | Cargo timing section | wall clock |
| rustc process duration | samply process metadata | wall clock |
| macro expansion | samply symbol attribution | CPU-weighted samples |
| resolution | samply symbol attribution | CPU-weighted samples |
| coherence | samply symbol attribution | CPU-weighted samples |
| type checking | samply symbol attribution | CPU-weighted samples |
| borrow checking and MIR transforms | samply symbol attribution | CPU-weighted samples |
| monomorphization | samply symbol attribution | CPU-weighted samples |
| metadata encoding | samply symbol attribution | CPU-weighted samples |
| code generation | samply symbol attribution | CPU-weighted samples |
| blocked and unattributed work | samply stack attribution | CPU-weighted samples |

Serial and parallel-codegen samples are distinct. Do not compare or combine
CPU-weighted phase counts with Cargo wall-clock section durations as though
they were the same measure.

## Build timeline

| field | source |
| --- | --- |
| active units | Cargo concurrency timeline |
| units waiting on dependencies | Cargo concurrency timeline |
| inactive units | Cargo concurrency timeline |
| whole-machine CPU percentage | Cargo CPU timeline |
| load average mean and maximum | client load sampler |
| CPU busy mean and maximum | client load sampler |
| Linux CPU, I/O, and memory pressure deltas | client load sampler |

Cargo concurrency and CPU series use different timestamps. Public timeline
rows therefore contain either the concurrency triple or CPU percentage, never
an artificial index-based join.

## Machine context

- machine id, unless explicitly omitted;
- CPU model and logical core count;
- memory size;
- operating system, version, kernel, and architecture;
- virtualization hint;
- Cargo version; and
- CI presence.

Hostname, username, and network identity are not read.

## Privacy and completeness

Cargo timing units and samply units pass the public-package classifier before
upload. Withheld units contribute no identity, timing, samples, settings, or
edges. `units_withheld` is the only retained fact about them.

A payload is complete only after the sampled Cargo command, timing parser, and
samply parser all succeed. The client does not create partial observations.

## Payload and publication

The schema-2 payload contains:

- `timings.unit_data`;
- `timings.concurrency_data`;
- `timings.cpu_usage`;
- `phases.units`;
- build, machine, and load context; and
- counts describing retained and withheld units.

It is encoded as zstd JSON and stored verbatim. Daily compaction derives the
five public parquet tables from these fields.
