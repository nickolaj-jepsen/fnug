from pathlib import Path

import click

from fnug.config import load_config
from fnug import FnugApp


@click.command()
@click.option("--config", "-c", type=click.Path(exists=True), default=".fnug.json", help="Config file")
def cli(config: str):
    file_path = Path(config)
    cwd = file_path.parent
    cfg = load_config(file_path)
    FnugApp(cfg, cwd=cwd).run()
