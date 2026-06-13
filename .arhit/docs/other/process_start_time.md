# process_start_time

daemon.rs: возвращает время старта процесса (секунды) для защиты от PID-recycling. macOS: proc_pidinfo PROC_PIDTBSDINFO -> proc_bsdinfo.pbi_start_tvsec. Linux: /proc/<pid>/stat field 22. None если процесс не существует/недоступен. Используется is_recorded_process_alive перед kill в stop/status/start.
