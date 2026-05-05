# Kirakira

> [!WARNING]
> This project is licensed under the GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later). Under AGPLv3 section 13, if you modify the Program, your modified version must "prominently offer all users interacting with it remotely through a computer network" an opportunity to receive the Corresponding Source of your version.
>
> 本项目依据 GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later) 授权。根据 AGPLv3 第 13 节，如果你修改本程序，你的修改版本必须“向所有通过计算机网络与其远程交互的用户醒目地提供”获取你版本的对应源代码的机会。
>
> このプロジェクトは GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later) の下でライセンスされています。AGPLv3 第 13 条に基づき、本プログラムを改変した場合、その改変版は「コンピュータネットワークを通じてリモートで対話するすべてのユーザーに対して目立つ形で」、あなたの版の対応するソースを受け取る機会を提供しなければなりません。

`Kirakira` is a lightweight, high-performance, modern KRKR game emulator.

## Usage

```sh
cargo run -p krkr-desktop
```

By default, the desktop app tries to launch the current directory when it
looks like a KRKR project. Otherwise it falls back to the directory containing
the executable.

You can pass a game/project directory explicitly:

```sh
cargo run -p krkr-desktop -- /path/to/game
```

A project directory is detected when it contains one of the following:

- `startup.tjs`
- `startup.ks`
- one or more `.xp3` archives in the project root
- one or more `.xp3` archives in `sys/`

For a release build:

```sh
cargo build -p krkr-desktop --release
./target/release/krkr-desktop /path/to/game
```

## Compatibility Notice

Kirakira is still under active development. Most KRKR plugin features are not
implemented yet. `Plugins.link` currently records linked plugin names and some
limited compatibility shims exist, but games that depend on native KRKR plugins
may fail or behave differently.
