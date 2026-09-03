import pathlib
import shutil
import tempfile
import time

out = pathlib.Path(__file__).resolve().parent
threshold = (out / 'instance.json').stat().st_birthtime
dest = out / 'runtime-logs'
dest.mkdir(exist_ok=True)
while not (out / 'stop-log-collector').exists():
    for root in pathlib.Path(tempfile.gettempdir()).glob('rustchan-e2e-*'):
        try:
            if root.stat().st_birthtime < threshold:
                continue
            source = root / 'server.log'
            if source.exists():
                shutil.copyfile(source, dest / (root.name + '.log'))
        except FileNotFoundError:
            pass
    time.sleep(1)
