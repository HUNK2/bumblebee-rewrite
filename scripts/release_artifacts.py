"""Prepare notices and inspect a Windows release without reading game media."""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import re
import struct
import urllib.parse
import urllib.request
import zipfile

PLAYER_FILES = {"bumblebee.exe", "Start-Bumblebee.ps1", "Start-Bumblebee.cmd", "README.md", "LICENSE", "THIRD-PARTY-NOTICES.md", "RELEASE-NOTES.md"}


def package_archive(staging_directory, archive):
    staging = Path(staging_directory)
    if {path.name for path in staging.iterdir()} != PLAYER_FILES or any(not path.is_file() or path.is_symlink() for path in staging.iterdir()):
        raise ValueError("Staging directory must contain only the explicit player file list")
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as output:
        for name in sorted(PLAYER_FILES):
            info = zipfile.ZipInfo(name, date_time=(2026, 1, 1, 0, 0, 0))
            info.create_system = 0
            info.compress_type = zipfile.ZIP_DEFLATED
            output.writestr(info, (staging / name).read_bytes(), compresslevel=9)


def notices(metadata_path, supplement_directory, output):
    metadata = json.loads(Path(metadata_path).read_text(encoding="utf-8-sig"))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    pending = [package["id"] for package in packages.values() if package["name"] == "bumblebee"]
    visited = set()
    while pending:
        package_id = pending.pop()
        if package_id in visited:
            continue
        visited.add(package_id)
        for dependency in nodes[package_id]["deps"]:
            if any(kind["kind"] != "dev" for kind in dependency["dep_kinds"]):
                pending.append(dependency["pkg"])
    third_party = sorted((packages[key] for key in visited if packages[key]["source"]), key=lambda item: (item["name"], item["version"]))
    sections = ["# Third-party notices\n", "Original game assets are not included. These notices cover open-source dependencies and their bundled resources.\n",
                "The Symphonia components are unmodified MPL-2.0 dependencies. Their exact source packages are linked below; the license is reproduced.\n",
                "## Dependency inventory\n", "| Package | Version | License expression | Source |\n|---|---|---|---|\n"]
    texts = {}
    missing = []
    for package in third_party:
        name, version = package["name"], package["version"]
        source_url = f"https://crates.io/api/v1/crates/{name}/{version}/download"
        sections.append(f"| {name} | {version} | {package['license'] or 'See license file'} | [Source]({source_url}) |\n")
        directory = Path(package["manifest_path"]).parent
        candidates = [path for path in directory.iterdir() if path.is_file() and re.match(r"^(?:licen[sc]e|copying|copyright|notice)(?:[._-]|$)", path.name, re.I)]
        if package.get("license_file"):
            candidates.append(directory / package["license_file"])
        for subdirectory in ("licenses", "license"):
            if (directory / subdirectory).is_dir():
                candidates.extend(path for path in (directory / subdirectory).rglob("*") if path.is_file())
        supplemental = Path(supplement_directory) / f"{name}-{version}"
        if supplemental.is_dir():
            candidates.extend(path for path in supplemental.iterdir() if path.is_file())
        if not candidates and package["license"] == "MPL-2.0":
            candidates = [Path(supplement_directory) / "MPL-2.0"]
        readable = 0
        for path in sorted(set(candidates)):
            if not path.is_file():
                continue
            try:
                content = path.read_text(encoding="utf-8-sig")
            except UnicodeError:
                continue
            if "\x00" in content or not content.strip():
                continue
            readable += 1
            digest = hashlib.sha256(content.encode()).hexdigest()
            entry = texts.setdefault(digest, {"content": content, "uses": []})
            entry["uses"].append(f"{name} {version}: {path.name}")
        if not readable:
            missing.append(f"{name} {version} ({package['license']})")
    sections.append("\n## Bundled font\n\nBevy's default Fira Mono subset uses the SIL Open Font License 1.1. Copyright attribution and license follow.\n\n")
    sections.append((Path(supplement_directory) / "Fira-LICENSE").read_text(encoding="utf-8-sig") + "\n")
    sections.append("\n## Dependency license and notice texts\n")
    for entry in texts.values():
        sections.append("\n### " + "; ".join(entry["uses"]) + "\n\n~~~~~~~~~~text\n" + entry["content"].rstrip() + "\n~~~~~~~~~~\n")
    Path(output).write_text("".join(sections), encoding="utf-8")
    print(json.dumps({"packages": len(third_party), "unique_notice_texts": len(texts), "missing_notice_files": missing}))
    if missing:
        raise SystemExit("Resolve missing notices before packaging.")


def fetch_missing(metadata_path, supplement_directory):
    metadata = json.loads(Path(metadata_path).read_text(encoding="utf-8-sig"))
    trees = {}
    def get(url):
        if urllib.parse.urlparse(url).hostname not in {"api.github.com", "gitlab.com"}:
            raise ValueError("Unexpected notice provider")
        request = urllib.request.Request(url, headers={"User-Agent": "release-notice-audit"})
        with urllib.request.urlopen(request, timeout=30) as response:
            return response.read()
    fetched = 0
    for package in metadata["packages"]:
        if not package["source"] or package["license"] == "MPL-2.0":
            continue
        completed = Path(supplement_directory) / f"{package['name']}-{package['version']}"
        if completed.is_dir() and any(completed.iterdir()):
            continue
        directory = Path(package["manifest_path"]).parent
        if any(path.is_file() and re.match(r"^(?:licen[sc]e|copying|copyright|notice)(?:[._-]|$)", path.name, re.I) for path in directory.iterdir()):
            continue
        if any((directory / folder).is_dir() for folder in ("license", "licenses")):
            continue
        vcs = json.loads((directory / ".cargo_vcs_info.json").read_text())
        commit = vcs["git"]["sha1"]
        relative = vcs.get("path_in_vcs", "").strip("/")
        ancestors = {""}
        if relative:
            parent = Path(relative)
            ancestors.update(str(item).replace("\\", "/") for item in (parent, *parent.parents) if str(item) != ".")
        parsed = urllib.parse.urlparse(package["repository"].rstrip("/").removesuffix(".git"))
        repository = parsed.path.strip("/")
        if parsed.hostname == "github.com":
            key = (repository, commit)
            if key not in trees:
                trees[key] = json.loads(get(f"https://api.github.com/repos/{repository}/git/trees/{commit}?recursive=1"))
            tree = trees[key]
            if tree.get("truncated"):
                raise ValueError("Upstream notice tree truncated")
            files = [entry for entry in tree["tree"] if entry["type"] == "blob"]
        elif parsed.hostname == "gitlab.com":
            project = urllib.parse.quote(repository, safe="")
            key = (repository, commit)
            if key not in trees:
                trees[key] = json.loads(get(f"https://gitlab.com/api/v4/projects/{project}/repository/tree?ref={commit}&per_page=100"))
            files = [entry for entry in trees[key] if entry["type"] == "blob"]
        else:
            raise ValueError("Missing notice from an unsupported repository host")
        candidates = [entry for entry in files if str(Path(entry["path"]).parent).replace("\\", "/").replace(".", "") in ancestors and re.match(r"^(?:licen[sc]e|copying|copyright|notice)(?:[._-]|$)", Path(entry["path"]).name, re.I)]
        if not candidates:
            license_expression = package["license"] or ""
            fallback_license = "Apache-2.0" if "Apache-2.0" in license_expression and "AND" not in license_expression and "WITH" not in license_expression else "CC0-1.0" if license_expression == "CC0-1.0" else None
            if fallback_license:
                destination = Path(supplement_directory) / f"{package['name']}-{package['version']}"
                destination.mkdir(parents=True, exist_ok=True)
                attribution = f"Upstream declares {license_expression}; {fallback_license} is selected for this dependency.\nAuthors: {', '.join(package.get('authors', []))}\nSource: {package['repository']} at {commit}\n\n"
                canonical = (Path(supplement_directory) / fallback_license).read_text(encoding="utf-8")
                (destination / fallback_license).write_text(attribution + canonical, encoding="utf-8")
                print(f"Prepared declared {fallback_license} notice: {package['name']}", flush=True)
                fetched += 1
                continue
            raise ValueError(f"No authoritative notice found for {package['name']}")
        destination = Path(supplement_directory) / f"{package['name']}-{package['version']}"
        destination.mkdir(parents=True, exist_ok=True)
        for index, entry in enumerate(candidates):
            if parsed.hostname == "github.com":
                blob = json.loads(get(entry["url"]))
                content = base64.b64decode(blob["content"])
            else:
                content = get(f"https://gitlab.com/api/v4/projects/{project}/repository/blobs/{entry['id']}/raw")
            content.decode("utf-8-sig")
            if b"\0" in content:
                raise ValueError("Unexpected binary notice")
            (destination / f"{index}-{Path(entry['path']).name}").write_bytes(content)
        fetched += 1
        print(f"Fetched exact-revision notices: {package['name']} {package['version']}", flush=True)
    print(f"Resolved notices for {fetched} packages.")


def executable_report(data, private_patterns):
    if data[:2] != b"MZ":
        raise ValueError("Executable is not a PE image")
    pe = struct.unpack_from("<I", data, 0x3C)[0]
    if data[pe:pe + 4] != b"PE\0\0":
        raise ValueError("Invalid PE signature")
    machine, section_count = struct.unpack_from("<HH", data, pe + 4)
    optional_size = struct.unpack_from("<H", data, pe + 20)[0]
    optional = pe + 24
    if machine != 0x8664 or struct.unpack_from("<H", data, optional)[0] != 0x20B:
        raise ValueError("Expected Windows x64 executable")
    sections = []
    for index in range(section_count):
        position = optional + optional_size + index * 40
        virtual_size, virtual_start, raw_size, raw_start = struct.unpack_from("<IIII", data, position + 8)
        sections.append((virtual_start, max(virtual_size, raw_size), raw_start))
    def offset(rva):
        for start, length, raw_start in sections:
            if start <= rva < start + length:
                return raw_start + rva - start
        raise ValueError("Unmapped PE RVA")
    import_rva = struct.unpack_from("<I", data, optional + 112 + 8)[0]
    imports = []
    if import_rva:
        position = offset(import_rva)
        while any(data[position:position + 20]):
            name_rva = struct.unpack_from("<I", data, position + 12)[0]
            start = offset(name_rva)
            imports.append(data[start:data.index(b"\0", start)].decode("ascii"))
            position += 20
    debug_rva, debug_size = struct.unpack_from("<II", data, optional + 112 + 6 * 8)
    decoded = [data.decode("latin1"), data.decode("utf-16-le", errors="ignore"), data[1:].decode("utf-16-le", errors="ignore")]
    patterns = [r"(?i)[A-Z]:[\\/]Users[\\/][^\s\x00]+", r"(?i)/(?:Users|home)/[^/\s\x00]+",
                r"(?i)gh[pousr]_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]{20,}"] + private_patterns
    for pattern in patterns:
        if any(re.search(pattern, text) for text in decoded):
            raise ValueError("Executable contains a private identifying path/marker; value withheld")
    debug_types = []
    pdb_reference = None
    if debug_rva:
        debug_start = offset(debug_rva)
        if debug_size % 28:
            raise ValueError("Malformed PE debug directory")
        for debug_index in range(debug_size // 28):
            debug_type = struct.unpack_from("<I", data, debug_start + debug_index * 28 + 12)[0]
            debug_types.append(debug_type)
            if debug_type == 2:
                record_size, _, record_offset = struct.unpack_from("<III", data, debug_start + debug_index * 28 + 16)
                record = data[record_offset:record_offset + record_size]
                if record[:4] != b"RSDS":
                    raise ValueError("Unexpected CodeView record")
                pdb_reference = record[24:].split(b"\0")[0].decode("utf-8")
                if pdb_reference != "bumblebee.pdb":
                    raise ValueError("Executable retains a non-generic PDB reference; inspect before release")
    return {"architecture": "Windows x64", "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest(), "imported_dlls": sorted(set(imports)), "debug_record_types": debug_types, "pdb_reference": pdb_reference, "pdb_reference_contains_path": False}


def audit(executable, archive, private_patterns):
    report = executable_report(Path(executable).read_bytes(), private_patterns)
    if archive:
        allowed = PLAYER_FILES
        with zipfile.ZipFile(archive) as package:
            names = package.namelist()
            if len(names) != len(allowed) or set(names) != allowed:
                raise ValueError("ZIP does not match the explicit release file list")
            if package.read("bumblebee.exe") != Path(executable).read_bytes():
                raise ValueError("ZIP executable differs from the inspected build")
            for name in names:
                if name == "bumblebee.exe":
                    continue
                text = package.read(name).decode("utf-8-sig")
                for pattern in private_patterns + [r"(?i)[A-Z]:[\\/]Users[\\/]", r"(?i)/(?:Users|home)/"]:
                    if re.search(pattern, text):
                        raise ValueError("ZIP text contains a private identifying marker; value withheld")
        report["zip_files"] = sorted(names)
        report["zip_sha256"] = hashlib.sha256(Path(archive).read_bytes()).hexdigest()
        report["zip_bytes"] = Path(archive).stat().st_size
    print(json.dumps(report, indent=2))


def main():
    parser = argparse.ArgumentParser()
    subparsers = parser.add_subparsers(dest="command", required=True)
    notice_parser = subparsers.add_parser("notices")
    notice_parser.add_argument("metadata")
    notice_parser.add_argument("supplements")
    notice_parser.add_argument("output")
    fetch_parser = subparsers.add_parser("fetch-missing")
    fetch_parser.add_argument("metadata")
    fetch_parser.add_argument("supplements")
    package_parser = subparsers.add_parser("package")
    package_parser.add_argument("staging")
    package_parser.add_argument("archive")
    audit_parser = subparsers.add_parser("audit")
    audit_parser.add_argument("executable")
    audit_parser.add_argument("--archive")
    audit_parser.add_argument("--private-pattern", action="append", default=[])
    args = parser.parse_args()
    if args.command == "notices":
        notices(args.metadata, args.supplements, args.output)
    elif args.command == "fetch-missing":
        fetch_missing(args.metadata, args.supplements)
    elif args.command == "package":
        package_archive(args.staging, args.archive)
    else:
        audit(args.executable, args.archive, args.private_pattern)


if __name__ == "__main__":
    main()
