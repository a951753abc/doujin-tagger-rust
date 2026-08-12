CREATE UNIQUE INDEX scan_runs_single_running
    ON scan_runs((1)) WHERE status = 'running';
