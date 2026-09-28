import pathlib
import tarfile
import zipfile

root = pathlib.Path(__file__).resolve().parents[1]
for path in (root / "dist").iterdir():
    if path.suffix == ".whl":
        with zipfile.ZipFile(path) as archive:
            names = archive.namelist()
        assert any(name.endswith("_native.pyi") for name in names)
        assert any(name.endswith("py.typed") for name in names)
    elif path.name.endswith(".tar.gz"):
        with tarfile.open(path) as archive:
            names = archive.getnames()
        assert any(name.endswith("sdk/src/session.rs") for name in names)
        assert any(name.endswith("Cargo.lock") for name in names)
        assert not any(name.endswith((".pyd", ".so", ".pdb")) for name in names)
    else:
        continue
    assert not any(
        pathlib.PurePosixPath(name).name.lower() == "agents.md" for name in names
    )
    assert not any(
        part in (".git", ".venv", "target")
        for name in names
        for part in pathlib.PurePosixPath(name).parts
    )
    print(path.name, len(names), "files: package audit passed")
