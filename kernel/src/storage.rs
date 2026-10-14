use crate::{fs, pci, smp, virtio_blk};

pub fn start() {
    let devices = pci::scan();
    let Some(dev) = virtio_blk::find(&devices) else {
        println!("storage: no virtio-blk disk on cpu {}", smp::core());
        return;
    };
    let Some(sectors) = virtio_blk::init(&dev) else {
        println!("storage: virtio-blk init failed");
        return;
    };
    println!("storage: virtio-blk on cpu {}, {} MiB", smp::core(), sectors * virtio_blk::SECTOR >> 20);
    fs::start();
}
