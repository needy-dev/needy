/*
 * The boot stub: an actor's PID 1 (`/.needy/init`), part of the base image.
 *
 * It hands the actor to the guest needy, which the daemon injects when the
 * actor starts (base ABI 1, see
 * docs/ai/references/architecture/2026-11-12-actor-images-design.md):
 *
 * - In a container the guest needy is bind-mounted at /.needy/needy already.
 * - In a VM, /.needy/needy is the base's empty placeholder: the stub mounts
 *   the guest disk (/dev/vdb, SquashFS) read-only at /.needy/disk and binds
 *   /.needy/disk/needy over the placeholder.
 *
 * Then it runs `/.needy/needy init` with its own environment. On any failure
 * it says what failed on stderr (the console) and, as a VM's PID 1, ends the
 * VM the way init does, so the run fails instead of hanging; otherwise it
 * exits 127.
 *
 * It depends on nothing but libc, so it never has to change with needy.
 * NEEDY_DIR may be set at build time (`-DNEEDY_DIR=...`) to try it against a
 * directory of one's own; the image uses /.needy.
 */
#include <errno.h>
#include <stdio.h>
#include <string.h>
#include <sys/mount.h>
#include <sys/reboot.h>
#include <sys/stat.h>
#include <unistd.h>

#ifndef NEEDY_DIR
#define NEEDY_DIR "/.needy"
#endif
#define GUEST NEEDY_DIR "/needy"
#define DISK_DIR NEEDY_DIR "/disk"
#define DISK_GUEST DISK_DIR "/needy"
#ifndef DISK
#define DISK "/dev/vdb"
#endif

static void fail(const char *what, const char *path, int error)
{
	fprintf(stderr, "needy: %s %s: %s\n", what, path, strerror(error));
	/*
	 * A VM ends on a reboot under Firecracker (x86_64, `reboot=k`) and on a
	 * power-off under Virtualization.framework (arm64), as init ends it
	 * (crates/core-actor/src/guest/init/mod.rs). A container's PID 1 lacks
	 * CAP_SYS_BOOT, so the call fails there and the stub exits.
	 */
	if (getpid() == 1) {
		sync();
#ifdef __aarch64__
		reboot(RB_POWER_OFF);
#else
		reboot(RB_AUTOBOOT);
#endif
	}
	_exit(127);
}

int main(int argc, char **argv, char **envp)
{
	struct stat guest;
	char *const init[] = { GUEST, "init", NULL };

	(void)argc;
	(void)argv;
	if (stat(GUEST, &guest) != 0)
		fail("cannot find the guest needy", GUEST, errno);
	if (guest.st_size == 0) {
		/*
		 * The base's placeholder: a VM, whose guest needy is on its disk,
		 * or a container the daemon did not inject one into.
		 */
		if (mount(DISK, DISK_DIR, "squashfs", MS_RDONLY | MS_NOSUID | MS_NODEV, NULL) != 0)
			fail("the guest needy is not injected (" GUEST " is the base image's empty placeholder), and cannot mount the guest disk",
			     DISK, errno);
		if (mount(DISK_GUEST, GUEST, NULL, MS_BIND, NULL) != 0)
			fail("cannot bind the guest needy over", GUEST, errno);
	}
	execve(GUEST, init, envp);
	fail("cannot run", GUEST, errno);
	return 127;
}
