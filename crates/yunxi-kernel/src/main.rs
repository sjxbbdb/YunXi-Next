use yunxi_kernel::YunxiKernel;

fn main() {
    let kernel = YunxiKernel::new();
    let snapshot = kernel.snapshot();
    println!(
        "YunXi kernel {} ready: {} plugins registered",
        env!("CARGO_PKG_VERSION"),
        snapshot.plugins().len()
    );
}
