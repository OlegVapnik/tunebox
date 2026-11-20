
use std::{ffi::c_void, ptr, sync::Arc, time::Duration};

use dispatch2::{DispatchQueue, DispatchTime};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use objc2_io_kit::{
    IOAllowPowerChange, IONotificationPort, IONotificationPortRef, IORegisterForSystemPower, io_connect_t, io_object_t,
    io_service_t, kIOMessageCanSystemSleep, kIOMessageSystemHasPoweredOn, kIOMessageSystemWillSleep,
};

use crate::power::Event;

const SLEEP_GRACE: Duration = Duration::from_secs(1);

type Callback = Arc<dyn Fn(Event) + Send + Sync>;

struct Watch {
    on_event: Callback,
    root_port: io_connect_t,
}

pub fn watch(on_event: Callback) -> bool {
    let watch = Box::into_raw(Box::new(Watch { on_event, root_port: 0 }));
    let mut port: IONotificationPortRef = ptr::null_mut();
    let mut notifier: io_object_t = 0;
    let root_port = unsafe { IORegisterForSystemPower(watch.cast(), &mut port, Some(changed), &mut notifier) };
    if root_port == 0 {
        tracing::warn!("cannot watch for sleep and wake");
        drop(unsafe { Box::from_raw(watch) });
        return false;
    }
    unsafe { (*watch).root_port = root_port };
    let source = unsafe { IONotificationPort::run_loop_source(port) };
    match (source, CFRunLoop::main()) {
        (Some(source), Some(main)) => main.add_source(Some(&source), unsafe { kCFRunLoopCommonModes }),
        _ => tracing::warn!("cannot receive sleep and wake notifications"),
    }
    true
}

unsafe extern "C-unwind" fn changed(data: *mut c_void, _: io_service_t, message: u32, argument: *mut c_void) {
    let watch = unsafe { &*data.cast::<Watch>() };
    let (root_port, request) = (watch.root_port, argument as isize);
    if message == kIOMessageCanSystemSleep {
        IOAllowPowerChange(root_port, request);
    } else if message == kIOMessageSystemWillSleep {
        (watch.on_event)(Event::Sleep);
        let allow = move || {
            IOAllowPowerChange(root_port, request);
        };
        match DispatchTime::try_from(SLEEP_GRACE) {
            Ok(when) => {
                let _ = DispatchQueue::main().after(when, allow);
            }
            Err(()) => allow(),
        }
    } else if message == kIOMessageSystemHasPoweredOn {
        (watch.on_event)(Event::Wake);
    }
}
