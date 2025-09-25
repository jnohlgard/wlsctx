#!/usr/bin/env python3

import socket
import os
import time
from dataclasses import dataclass
from functools import partial
from tempfile import TemporaryDirectory
from xdg_base_dirs import xdg_runtime_dir
from pywayland.client import Display
from pywayland.protocol.security_context_v1 import WpSecurityContextManagerV1, WpSecurityContextV1

@dataclass
class SecurityContextExt:
    manager: None | WpSecurityContextManagerV1.global_class = None

def registry_global_handler(ext, registry, id_, interface, version):
    if interface == WpSecurityContextManagerV1.name:
        print(f"got security context manager {version}")
        ext.manager = registry.bind(id_, WpSecurityContextManagerV1, version)

def main():
    with TemporaryDirectory(dir=xdg_runtime_dir(), prefix='isol-') as rundir:
        instance_id = os.path.basename(rundir).removeprefix("isol-")
        with Display() as display:
            registry = display.get_registry()
            ext = SecurityContextExt()
            registry.dispatcher["global"] = partial(registry_global_handler, ext)
            display.dispatch()
            display.roundtrip()
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM, 0) as listen_sock:
                listen_sock.bind(f"{rundir}/wayland-1")
                listen_sock.listen()
                sync_fds = os.pipe2(os.O_CLOEXEC)
                close_fd = sync_fds[1]
                security_context = ext.manager.create_listener(listen_sock.fileno(), close_fd)
                security_context.set_sandbox_engine('io.r5t')
                security_context.set_app_id('sandbox.devenv')
                security_context.set_instance_id(instance_id)
                security_context.commit()
                security_context.destroy()
                if display.roundtrip() < 0:
                    raise RuntimeError("Roundtrip failed")
        print(f"rundir: {rundir}/")


if __name__ == '__main__':
    main()
