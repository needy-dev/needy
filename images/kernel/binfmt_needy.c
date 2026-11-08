// SPDX-License-Identifier: GPL-2.0
/*
 * Route selected executables to a proxy, in the kernel, without ptrace.
 *
 * Runs ahead of binfmt_elf and binfmt_script. When the executable named by
 * the exec matches the route list, the binary is swapped for the proxy, as
 * binfmt_script swaps a script for its interpreter, but argv is left
 * untouched. The proxy finds the original path in AT_EXECFN.
 *
 * Only execs by `route_uid` are routed, so the guest's own init stays local,
 * and execs made by the proxy itself are never routed, so the proxy can run
 * the real binary when a rule keeps the command on the actor.
 */
#include <linux/binfmts.h>
#include <linux/dcache.h>
#include <linux/err.h>
#include <linux/fs.h>
#include <linux/init.h>
#include <linux/module.h>
#include <linux/file.h>
#include <linux/mm.h>
#include <linux/sched/mm.h>
#include <linux/cred.h>
#include <linux/string.h>
#include <linux/uidgid.h>

static char route[512];
static char proxy[256] = "/.needy/proxy";
module_param_string(route, route, sizeof(route), 0600);
static int route_uid = -1;
module_param_string(proxy, proxy, sizeof(proxy), 0600);
module_param(route_uid, int, 0600);

static bool caller_is_proxy(void)
{
	struct file *exe = get_mm_exe_file(current->mm);
	bool same = false;

	if (exe) {
		struct name_snapshot name;

		take_dentry_name_snapshot(&name, exe->f_path.dentry);
		same = !strcmp(name.name.name, kbasename(proxy)) &&
		       exe->f_path.dentry->d_parent &&
		       !strcmp(exe->f_path.dentry->d_parent->d_name.name, ".needy");
		release_dentry_name_snapshot(&name);
		fput(exe);
	}
	return same;
}

static bool listed(const char *name)
{
	size_t n = strlen(name);
	const char *p = route;

	/* A lone "*" routes every exec. */
	if (route[0] == '*' && !route[1])
		return true;
	while (n && *p) {
		const char *end = strchrnul(p, ',');

		if ((size_t)(end - p) == n && !strncmp(p, name, n))
			return true;
		p = *end ? end + 1 : end;
	}
	return false;
}

static int load_needy(struct linux_binprm *bprm)
{
	struct name_snapshot dname;
	struct file *file;
	bool hit;
	int ret;

	if (!route[0] || !strcmp(bprm->interp, proxy))
		return -ENOEXEC;
	if (route_uid >= 0 &&
	    !uid_eq(current_uid(), make_kuid(&init_user_ns, route_uid)))
		return -ENOEXEC;
	if (caller_is_proxy())
		return -ENOEXEC;

	/* Match the exec path, its basename, or the file name for fd and memfd execs. */
	take_dentry_name_snapshot(&dname, bprm->file->f_path.dentry);
	hit = listed(bprm->interp) || listed(kbasename(bprm->interp)) ||
	      listed(dname.name.name);
	release_dentry_name_snapshot(&dname);
	if (!hit)
		return -ENOEXEC;

	ret = bprm_change_interp(proxy, bprm);
	if (ret)
		return ret;
	file = open_exec(proxy);
	if (IS_ERR(file))
		return PTR_ERR(file);
	bprm->interpreter = file;
	return 0;
}

static struct linux_binfmt needy_format = {
	.module = THIS_MODULE,
	.load_binary = load_needy,
};

static int __init init_needy_binfmt(void)
{
	insert_binfmt(&needy_format);
	return 0;
}
core_initcall(init_needy_binfmt);
MODULE_LICENSE("GPL");
