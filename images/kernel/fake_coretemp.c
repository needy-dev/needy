// Fake Intel coretemp: same platform device name and labels as the real driver.
//
// Source: https://github.com/torvalds/linux/blob/v6.18/drivers/hwmon/coretemp.c (Linux v6.18):
// device and hwmon name "coretemp" (#L32, #L626, #L692-L693); labels
// "Package id %u" and "Core %u" (#L335-L345), where the core number is
// `topology_core_id()` (#L512); attributes input, crit, crit_alarm and label
// on every sensor and max where the MSR gives it (#L44-L53, #L569-L580).
// Reference: https://github.com/torvalds/linux/blob/v6.18/Documentation/ABI/testing/sysfs-class-hwmon?plain=1#L440-L455
// (temp*_input and temp*_crit in millidegrees Celsius), #L385-L391 (max),
// #L457-L464 (crit_alarm), #L536-L544 (label).
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/hwmon.h>
#include <linux/sched/loadavg.h>
#include <linux/random.h>
#include <linux/cpumask.h>

/* The OptiPlex 7000 (i7-12700) `sensors` logs these values come from:
 * https://github.com/linuxhw/Sensors/tree/2b4210eb2e8686d7841e8919b019c7e34a1bdb6c/Desktop/Dell/OptiPlex/OptiPlex%207000
 * The 38 C idle default is "Package id 0: +38.0 C" in the Ubuntu 22.04 log,
 * https://github.com/linuxhw/Sensors/blob/2b4210eb2e8686d7841e8919b019c7e34a1bdb6c/Desktop/Dell/OptiPlex/OptiPlex%207000/7241B617E4E3/UBUNTU-22.04/5.17.0-051700-GENERIC/X86_64/6AAE7C23AD
 * Recipes override `base_mc` (`crates/core-actor/src/guest/init/identity.rs`). */
static int base_mc = 38000;      /* idle temperature, millidegrees C */
static int override_mc;          /* chaos: force every sensor to this value */
module_param(base_mc, int, 0644);
module_param(override_mc, int, 0644);

static struct platform_device *pdev;
static struct device *hwdev;
static int ncores;

/* Core IDs step by 4 (0, 4, 8, ...) as the i7-12700's performance cores do
 * in three of the logs above ("Core 0" to "Core 28" in steps of 4); the
 * fourth, a 6-core CPU, numbers Core 0 to 5. The labels are
 * `topology_core_id()` (coretemp.c#L512 above). */
#define CORE_ID_STEP 4
#define MAX_CORES 64
/* The real driver puts the package in temp1 and Core N in temp(N+2)
 * (https://github.com/torvalds/linux/blob/v6.18/drivers/hwmon/coretemp.c#L427-L436);
 * hwmon names channel ch temp(ch+1), so Core N is channel N+1 and the
 * channels between core IDs exist but are hidden. */
#define MAX_CHANNEL ((MAX_CORES - 1) * CORE_ID_STEP + 1)

static bool is_core(int ch)
{
	return ch >= 1 && (ch - 1) % CORE_ID_STEP == 0 && (ch - 1) / CORE_ID_STEP < ncores;
}

/* 0 for the package, 1 for the first core, 2 for the second, ... */
static int sensor_index(int ch)
{
	return ch == 0 ? 0 : (ch - 1) / CORE_ID_STEP + 1;
}

/* Idle plus load, a step per core and noise, in whole degrees.
 * The real driver reads whole degrees: TjMax minus the digital readout times
 * 1000, https://github.com/torvalds/linux/blob/v6.18/drivers/hwmon/coretemp.c#L408,
 * and every log above shows ".0".
 * NOTICE: 6 C per unit of 1-minute load (at most 50 C), 0.7 C per core and
 * under 1 C of noise are needy's model, not a measurement; the logs only show
 * idle cores spread over 5 C (32 to 37 C). With 8 cores at the 38 C default,
 * the hottest reading is 38 + 50 + 5.6 + 1 < 100 C, below crit. */
static long reading(int channel)
{
	/* `avenrun` is fixed point with FSHIFT fraction bits, so the shift is
	 * LOAD_INT(): https://github.com/torvalds/linux/blob/v6.18/include/linux/sched/loadavg.h#L15-L23 and #L43-L44 */
	unsigned long load = avenrun[0] >> FSHIFT;       /* 1-minute load, integer part */
	long t = base_mc + min_t(long, load * 6000, 50000) + channel * 700 + get_random_u32_below(1000);
	return override_mc ? override_mc : t / 1000 * 1000;
}

static umode_t visible(const void *d, enum hwmon_sensor_types type, u32 attr, int ch)
{
	return ch == 0 || is_core(ch) ? 0444 : 0;
}

static int read(struct device *dev, enum hwmon_sensor_types type, u32 attr, int ch, long *val)
{
	switch (attr) {
	case hwmon_temp_input: *val = reading(sensor_index(ch)); return 0;
	/* The real driver reads crit (TjMax) and max (TjMax minus the target
	 * offset) from MSR IA32_TEMPERATURE_TARGET: https://github.com/torvalds/linux/blob/v6.18/drivers/hwmon/coretemp.c#L268-L328.
	 * 80 C and 100 C are every sensor's "high = +80.0 C, crit = +100.0 C" in
	 * all four OptiPlex 7000 logs above. */
	case hwmon_temp_max:   *val = 80000; return 0;
	case hwmon_temp_crit:  *val = 100000; return 0;
	case hwmon_temp_crit_alarm: *val = reading(sensor_index(ch)) >= 100000; return 0;
	}
	return -EOPNOTSUPP;
}

static int read_string(struct device *dev, enum hwmon_sensor_types type, u32 attr, int ch, const char **str)
{
	/* Channel ch is Core ch-1 (see MAX_CHANNEL). */
	static char labels[MAX_CORES + 1][16];
	int i = sensor_index(ch);
	if (ch == 0) snprintf(labels[0], 16, "Package id 0");
	else snprintf(labels[i], 16, "Core %d", ch - 1);
	*str = labels[i];
	return 0;
}

static const struct hwmon_ops ops = { .is_visible = visible, .read = read, .read_string = read_string };
static u32 temp_cfg[MAX_CHANNEL + 2]; /* zero-terminated */
static struct hwmon_channel_info temp_info = { .type = hwmon_temp, .config = temp_cfg };
static const struct hwmon_channel_info *infos[] = { &temp_info, NULL };
static const struct hwmon_chip_info chip = { .ops = &ops, .info = infos };

static int __init fake_init(void)
{
	int i;
	ncores = min_t(int, num_online_cpus(), MAX_CORES);
	/* hwmon stops at the first zero config, so the hidden channels between
	 * cores need one too; `visible` hides them. */
	for (i = 0; i <= (ncores - 1) * CORE_ID_STEP + 1; i++)
		temp_cfg[i] = HWMON_T_INPUT | HWMON_T_LABEL | HWMON_T_MAX | HWMON_T_CRIT | HWMON_T_CRIT_ALARM;
	pdev = platform_device_register_simple("coretemp", 0, NULL, 0);
	if (IS_ERR(pdev)) return PTR_ERR(pdev);
	hwdev = hwmon_device_register_with_info(&pdev->dev, "coretemp", NULL, &chip, NULL);
	if (IS_ERR(hwdev)) { platform_device_unregister(pdev); return PTR_ERR(hwdev); }
	return 0;
}

static void __exit fake_exit(void) { hwmon_device_unregister(hwdev); platform_device_unregister(pdev); }
module_init(fake_init);
module_exit(fake_exit);
MODULE_LICENSE("GPL");
