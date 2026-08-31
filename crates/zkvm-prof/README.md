# zkvm-prof

Prices one execution of a guest on a zkVM, and reads the peak heap that execution reached.

The library takes a guest ELF as bytes and an input as bytes. It knows nothing about what a guest
computes or what it is handed, so it profiles any guest built for a zkVM it supports. Fetching an
ELF, encoding an input and writing the results out are the caller's.

## What prices a guest

An execution is priced by `ere-server`, which the pinned ere revision publishes one image of per
zkVM. The library pulls that image, starts it against the guest ELF, and reads back what ere's own
estimator charged. Nothing here links a zkVM SDK, so a zkVM is added by bumping the ere revision
rather than by adding a backend.

| zkVM | Cost | Peak heap |
| --- | --- | --- |
| OpenVM | Trace cells, summed over the segments a metered run reports | Read from guest memory |
| SP1 | Gas without the division that rounds it, which keeps the weighted sum exact | Read from the file behind guest memory |
| ZisK | The weighted sum the emulator accumulates | Read from guest memory |

SP1 keeps guest memory behind its executor except on the target that executor compiles a native JIT
for, so its heap is read out of the file that JIT runs the guest against. The image is `x86_64` Linux,
so that reading is the same whatever host runs the container.

## Use

```rust
use zkvm_prof::{Profiler, zkVMKind};

let elf = std::fs::read("guest.elf")?;
let input = std::fs::read("input.bin")?;

let profiler = Profiler::new(zkVMKind::Zisk, &elf)?;
let execution = profiler.profile(&input)?;
println!("{}", execution.cost.values().sum::<u64>());
println!("{:?}", execution.peak_heap_bytes);
```

A profiler starts one container and hands it the guest, so whatever the zkVM derives from that ELF is
paid for once and every input after the first is one call into a warm process. It is `Sync`, so one
profiler serves many threads.

Build one profiler per process, and profile one guest of a zkVM per host at a time. A container is
named after its zkVM and binds one port, so a profiler that finds one of its zkVM already running
stops rather than taking it over.

`profile` is the whole of the interface. A guest that breaks a zkVM invariant panics inside the
container, which reports it as the error it is, so one bad input costs a caller that input and
nothing more.

## Cost kinds

`Execution::cost` is an open map of whatever kinds the zkVM charges for. Those kinds partition the
execution, so they sum to what the whole of it cost and no key holds that whole. `composition(zkvm)`
names them in the order a chart stacks them, and each one carries the note a report prints under that
chart.

ere names the keys and this crate names what each covers, so a run is checked against that list. A
kind an ere revision adds stops a run rather than reaching a chart unlabelled.

## Build

The host needs the `docker` command and permission to run containers. `ERE_IMAGE_REGISTRY` names the
registry the images are pulled from, and defaults to the one ere publishes to. No zkVM toolchain is
needed, the images carrying their own.
