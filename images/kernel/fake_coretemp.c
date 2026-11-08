// Fake Intel coretemp: same platform device name and labels as the real driver.
#include <linux/module.h>
#include <linux/platform_device.h>
#include <linux/hwmon.h>
#include <linux/sched/loadavg.h>
#include <linux/random.h>
#include <linux/cpumask.h>

static int base_mc = 38000;      /* idle temperature, millidegrees C */
static int override_mc;          /* chaos: force every sensor to this value */
module_param(base_mc, int, 0644);
module_param(override_mc, int, 0644);

static struct platform_device *pdev;
static struct device *hwdev;
static int ncores;

static long reading(int channel)
{
	unsigned long load = avenrun[0] >> FSHIFT;       /* 1-minute load, integer part */
	long t = base_mc + min_t(long, load * 6000, 50000) + channel * 700 + get_random_u32_below(1500);
	return override_mc ? override_mc : t;
}

static umode_t visible(const void *d, enum hwmon_sensor_types type, u32 attr, int ch) { return 0444; }

static int read(struct device *dev, enum hwmon_sensor_types type, u32 attr, int ch, long *val)
{
	switch (attr) {
	case hwmon_temp_input: *val = reading(ch); return 0;
	case hwmon_temp_max:   *val = 80000; return 0;
	case hwmon_temp_crit:  *val = 100000; return 0;
	case hwmon_temp_crit_alarm: *val = reading(ch) >= 100000; return 0;
	}
	return -EOPNOTSUPP;
}

static int read_string(struct device *dev, enum hwmon_sensor_types type, u32 attr, int ch, const char **str)
{
	static char labels[65][16];
	if (ch == 0) snprintf(labels[0], 16, "Package id 0");
	else snprintf(labels[ch], 16, "Core %d", (ch - 1) * 4);
	*str = labels[ch];
	return 0;
}

static const struct hwmon_ops ops = { .is_visible = visible, .read = read, .read_string = read_string };
static u32 temp_cfg[66];
static struct hwmon_channel_info temp_info = { .type = hwmon_temp, .config = temp_cfg };
static const struct hwmon_channel_info *infos[] = { &temp_info, NULL };
static const struct hwmon_chip_info chip = { .ops = &ops, .info = infos };

static int __init fake_init(void)
{
	int i;
	ncores = min_t(int, num_online_cpus(), 64);
	for (i = 0; i <= ncores; i++)
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
