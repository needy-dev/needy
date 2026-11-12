/*
 * The shim stub: `/.needy/proxy` in the base image, and in a container image
 * every program on PATH, as a hard link to it.
 *
 * It hands the exec to the guest needy, which the daemon injects at
 * /.needy/needy when the actor starts (base ABI 1, see
 * docs/ai/references/architecture/2026-11-12-actor-images-design.md):
 *
 *   execve("/.needy/needy", <argv, unchanged>, <environment>
 *          + NEEDY_STUB_EXE=<readlink /proc/self/exe>
 *          + NEEDY_STUB_EXECFN=<getauxval(AT_EXECFN)>)
 *
 * argv stays the caller's, argv[0] included, since programs such as busybox
 * and python3 behave by the name they were called by. NEEDY_STUB_EXE is the
 * path the stub was started through: a shim's program path, or
 * /.needy/proxy when the kernel's binfmt_needy or a route stub started it.
 * NEEDY_STUB_EXECFN is the path the original execve named, which the
 * re-exec would replace. The guest needy removes both before it runs
 * anything. Values already in the environment are replaced.
 *
 * When the guest needy is not injected, /.needy/needy is the base image's
 * empty placeholder and execve fails with ENOEXEC. That is an image build
 * (a `RUN` in an image `FROM` the base, whose /bin/sh is a shim) or the
 * image run without needy, and nothing routes there: a shim then runs its
 * real program, /.needy/real/<its path>, with the caller's argv and
 * environment, as the guest needy does for a name it does not route.
 * /.needy/proxy itself stands for no program, so it still fails.
 *
 * On a failure it says what failed and exits 127, as a shell does for a
 * command it cannot run.
 *
 * It depends on nothing but libc, so it never has to change with needy.
 * NEEDY_DIR may be set at build time (`-DNEEDY_DIR=...`) to try it against a
 * directory of one's own; the image uses /.needy.
 */
#include <errno.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/auxv.h>
#include <unistd.h>

#ifndef NEEDY_DIR
#define NEEDY_DIR "/.needy"
#endif
#define GUEST NEEDY_DIR "/needy"
#define REAL NEEDY_DIR "/real"
#define EXE_VAR "NEEDY_STUB_EXE="
#define EXECFN_VAR "NEEDY_STUB_EXECFN="

static char *var(const char *name, const char *value)
{
	size_t n = strlen(name), m = strlen(value);
	char *s = malloc(n + m + 1);

	if (!s) {
		fputs("needy: out of memory\n", stderr);
		_exit(127);
	}
	memcpy(s, name, n);
	memcpy(s + n, value, m + 1);
	return s;
}

int main(int argc, char **argv, char **envp)
{
	char exe[PATH_MAX];
	ssize_t len = readlink("/proc/self/exe", exe, sizeof(exe) - 1);
	const char *execfn = (const char *)getauxval(AT_EXECFN);
	const char *name = argc > 0 && argv[0] ? argv[0] : "";
	size_t count = 0, kept = 0;
	char **env;

	if (len < 0) {
		fprintf(stderr, "needy: cannot run %s: cannot read /proc/self/exe: %s\n", name, strerror(errno));
		return 127;
	}
	exe[len] = '\0';
	if (!execfn)
		execfn = name;
	while (envp[count])
		count++;
	env = malloc((count + 3) * sizeof(*env));
	if (!env) {
		fputs("needy: out of memory\n", stderr);
		return 127;
	}
	for (size_t i = 0; i < count; i++) {
		if (!strncmp(envp[i], EXE_VAR, sizeof(EXE_VAR) - 1) ||
		    !strncmp(envp[i], EXECFN_VAR, sizeof(EXECFN_VAR) - 1))
			continue;
		env[kept++] = envp[i];
	}
	env[kept++] = var(EXE_VAR, exe);
	env[kept++] = var(EXECFN_VAR, execfn);
	env[kept] = NULL;
	execve(GUEST, argv, env);
	/* ENOEXEC: the base image's empty placeholder is still there. */
	if (errno == ENOEXEC && strncmp(exe, NEEDY_DIR "/", sizeof(NEEDY_DIR)) != 0) {
		char real[PATH_MAX];

		if (snprintf(real, sizeof(real), "%s%s", REAL, exe) < (int)sizeof(real)) {
			execve(real, argv, envp);
			if (errno != ENOENT) {
				fprintf(stderr, "needy: cannot run %s: %s: %s\n", name, real, strerror(errno));
				return 127;
			}
		}
		errno = ENOEXEC;
	}
	fprintf(stderr, "needy: cannot run %s: %s: %s%s\n", name, GUEST, strerror(errno),
		errno == ENOENT || errno == ENOEXEC ? " (the daemon did not inject the guest needy)" : "");
	return 127;
}
