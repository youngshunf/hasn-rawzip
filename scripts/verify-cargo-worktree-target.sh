#!/usr/bin/env bash
# 外置 cargo target 守卫：核对「当前 worktree 根」与 `CARGO_TARGET_DIR` 是否配对。
#
# 判据：父仓 CLAUDE.md「Rust 多 Worktree 外置盘编译产物」。每个 worktree 必须用自己的 target
# 目录——共用会跨 worktree 互相判 stale 触发全量重编，`cargo test` 甚至可能跑到别的分支编出的
# 旧二进制。**肉眼看 basename 会漏，这个脚本是唯一可靠的判据。**
#
# 用法：
#   bash scripts/verify-cargo-worktree-target.sh              # 校验当前环境
#   bash scripts/verify-cargo-worktree-target.sh --self-test  # 反例自测：证明守卫现在还能红
#
# 环境变量：
#   CARGO_TARGET_DIR                       被校验对象。一律取 `use-ext-cargo` 的输出，**禁止自己算 hash**
#   CARGO_WORKTREE_TARGET_ROOT             钉死唯一 target 根（设了就不走下面的候选回落）
#   CARGO_WORKTREE_TARGET_ROOT_CANDIDATES  冒号分隔的候选根，覆盖默认顺序
#
# 默认候选根，按顺序取**存在**的那些：
#   1. /Volumes/ExtraData/cargo-targets   外置盘
#   2. $HOME/cargo-targets                外置盘未挂载时 `use-ext-cargo` 的回落落点
#
# ⚠️ 候选回落是 2026-08-12（R0-a）补的。在那之前脚本硬编码外置盘并 `[[ ! -d ]]` 直接 fail：
# 外置盘一没挂载，这个「跑 cargo 前的固定前置」本身就跑不了，实际做法只能是绕过它——
# **守卫绕不过去才有价值**。所以两个根都认，但**两个根同时存在却用了回落根**时必须警告：
# 那意味着同一个 worktree 在两块盘上各留一份 target，缓存分叉、各付一次冷构建。
set -euo pipefail

# 必须解析成绝对路径：自测会 cd 进 fixture worktree 再把本脚本当子进程跑，
# 相对路径在那里找不到文件（会得到 127 而不是守卫的判定结果）。
SELF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)/$(basename "${BASH_SOURCE[0]}")"

fail() {
  echo "[cargo-target-guard] 失败：$*" >&2
  exit 1
}

warn() {
  echo "[cargo-target-guard] 警告：$*" >&2
}

# 路径哈希：与 `use-ext-cargo` 的映射规则一致（对 worktree 绝对路径取 sha1 前 12 位）。
path_hash() {
  if command -v shasum >/dev/null 2>&1; then
    printf '%s\n' "$1" | shasum | awk '{print $1}'
    return
  fi
  if command -v sha1sum >/dev/null 2>&1; then
    printf '%s\n' "$1" | sha1sum | awk '{print $1}'
    return
  fi
  fail "系统缺少 shasum 或 sha1sum，无法计算 worktree 路径哈希"
}

# ── 候选 target 根 ──────────────────────────────────────────────────────────
resolve_roots() {
  local raw=()
  if [[ -n "${CARGO_WORKTREE_TARGET_ROOT:-}" ]]; then
    raw=("${CARGO_WORKTREE_TARGET_ROOT}")
    ROOT_SOURCE="CARGO_WORKTREE_TARGET_ROOT"
  elif [[ -n "${CARGO_WORKTREE_TARGET_ROOT_CANDIDATES:-}" ]]; then
    local IFS=:
    read -r -a raw <<<"${CARGO_WORKTREE_TARGET_ROOT_CANDIDATES}"
    ROOT_SOURCE="CARGO_WORKTREE_TARGET_ROOT_CANDIDATES"
  else
    raw=("/Volumes/ExtraData/cargo-targets" "${HOME}/cargo-targets")
    ROOT_SOURCE="默认候选（外置盘优先，内置盘回落）"
  fi

  ALL_ROOTS=()
  EXISTING_ROOTS=()
  local r
  for r in "${raw[@]}"; do
    [[ -n "$r" ]] || continue
    ALL_ROOTS+=("$r")
    # 必须写成 if，不能写 `[[ -d ]] && push`：候选根全都不存在时，函数的最后一条命令
    # 返回 1，`set -e` 会**静默终止整个脚本**（无任何输出），把「候选根全缺失」这条判定
    # 变成一次无声崩溃。实测踩过。
    if [[ -d "$r" ]]; then
      EXISTING_ROOTS+=("$(cd "$r" && pwd -P)")
    fi
  done
}

main() {
  local worktree_root
  if ! worktree_root="$(git rev-parse --show-toplevel 2>/dev/null)"; then
    fail "当前目录不在 Git worktree 中"
  fi
  worktree_root="$(cd "${worktree_root}" && pwd -P)"

  resolve_roots
  if [[ "${#EXISTING_ROOTS[@]}" -eq 0 ]]; then
    fail "候选 Cargo target 根一个都不存在（来源：${ROOT_SOURCE}）：$(printf '%s ' "${ALL_ROOTS[@]}")
外置盘没挂载时请先创建回落根：mkdir -p \"\${HOME}/cargo-targets\""
  fi

  # 输出由守卫自身计算的首选 target，供 CI 配置使用，不解析失败文案。
  if [[ "${1:-}" == "--print-target" ]]; then
    local print_hash
    print_hash="$(path_hash "${worktree_root}")"
    printf '%s/%s-%s\n' "${EXISTING_ROOTS[0]}" "$(basename "${worktree_root}")" "${print_hash:0:12}"
    return 0
  fi

  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    fail "CARGO_TARGET_DIR 为空；请先执行 source ~/.zshrc && use-ext-cargo"
  fi
  local target_parent
  target_parent="$(dirname "${CARGO_TARGET_DIR}")"
  if [[ ! -d "${target_parent}" ]]; then
    fail "CARGO_TARGET_DIR 的父目录不存在：${target_parent}"
  fi
  local actual
  actual="$(cd "${target_parent}" && pwd -P)/$(basename "${CARGO_TARGET_DIR}")"

  local worktree_name worktree_hash suffix
  worktree_name="$(basename "${worktree_root}")"
  worktree_hash="$(path_hash "${worktree_root}")"
  suffix="${worktree_name}-${worktree_hash:0:12}"

  # ── 先判「复用了主 clone 的 target」，再判一般性不匹配 ──────────────────────
  #
  # ⚠️ 顺序是 2026-08-12（R0-a）修正的。原实现把这段放在一般性不匹配判定**之后**，
  # 于是它是**死代码**：linked worktree 用了主 clone 的 target，basename 必然也对不上，
  # 一定先被上面那条拦掉，这条专用信息一次都打不出来（自测 C8 复现）。
  # 而这恰好是父仓 CLAUDE.md 记的头号坑——非交互 shell 的 `cd` 不触发 chpwd 钩子，
  # 环境变量又跨命令残留，于是「人在 worktree 里、CARGO_TARGET_DIR 还指着主 clone」。
  # 给它一条准确的诊断比让它退化成泛泛的「不匹配」有用得多。
  local common_dir main_clone_root main_name main_hash main_target root
  common_dir="$(git rev-parse --git-common-dir)"
  if [[ "${common_dir}" != /* ]]; then
    common_dir="${worktree_root}/${common_dir}"
  fi
  common_dir="$(cd "${common_dir}" && pwd -P)"
  main_clone_root="$(dirname "${common_dir}")"
  if [[ "${worktree_root}" != "${main_clone_root}" ]]; then
    main_name="$(basename "${main_clone_root}")"
    main_hash="$(path_hash "${main_clone_root}")"
    for root in "${EXISTING_ROOTS[@]}"; do
      main_target="${root}/${main_name}-${main_hash:0:12}"
      if [[ "${actual}" == "${main_target}" ]]; then
        fail "linked worktree 复用了主 clone 的 Cargo target：${main_target}
当前 worktree 是 ${worktree_root}，应使用 ${EXISTING_ROOTS[0]}/${suffix}。
多半是 CARGO_TARGET_DIR 跨命令残留了——每条 cargo 命令都要重新 export。"
      fi
    done
  fi

  # 逐个候选根算期望值；命中任意一个即通过。
  local expected_list=() matched_root="" matched_index=-1 i=0
  for root in "${EXISTING_ROOTS[@]}"; do
    expected_list+=("${root}/${suffix}")
    if [[ "${actual}" == "${root}/${suffix}" ]]; then
      matched_root="${root}"
      matched_index=$i
    fi
    i=$((i + 1))
  done

  if [[ -z "${matched_root}" ]]; then
    fail "CARGO_TARGET_DIR 与当前 worktree 不匹配；期望 $(printf '%s 或 ' "${expected_list[@]}" | sed 's/ 或 $//')，实际 ${actual}"
  fi

  # 用了非首选根，而首选根也在，说明同一 worktree 的 target 分散在两块盘上。
  if [[ "${matched_index}" -gt 0 ]]; then
    warn "命中的是回落根 ${matched_root}，但更靠前的候选根 ${EXISTING_ROOTS[0]} 也存在。
同一个 worktree 会在两块盘上各留一份 target，缓存分叉、各付一次冷构建。
确认这是有意为之（例如外置盘刚挂上、暂不迁移）再继续。"
  fi

  local branch
  branch="$(git branch --show-current)"
  echo "[cargo-target-guard] 通过"
  echo "worktree_root=${worktree_root}"
  echo "branch=${branch:-DETACHED}"
  echo "target_root=${matched_root}（来源：${ROOT_SOURCE}）"
  echo "CARGO_TARGET_DIR=${actual}"
}

# ── 反例自测 ────────────────────────────────────────────────────────────────
#
# 09 号 §2 记录的守卫失效方式是**静默恒真**，不是报错。所以自测必须逐条造违规、断言它红，
# 而不是只跑一遍正例看它绿。
#
# 自测把本脚本当**子进程**跑（不是复制一份判定逻辑），因此测的就是线上那份实现。
# 期望值不自己算 hash——从守卫自己的失败信息里抠出来，顺带证明「失败时打印期望值与实际值」
# 这条建仓许可 node-worktree-target-guard 明确要求的行为还在。
self_test() {
  local tmp failures=0
  # 必须 `pwd -P` 归一化：macOS 上 `mktemp -d` 给的是 /var/folders/…，而守卫内部一律
  # 归一化成 /private/var/folders/…。不归一化就会拿两种写法互相比较，断言全部误判。
  tmp="$(cd "$(mktemp -d)" && pwd -P)"
  # shellcheck disable=SC2064
  trap "rm -rf '${tmp}'" EXIT

  local ext="${tmp}/roots/ext" home_root="${tmp}/roots/home" wt="${tmp}/wt"
  mkdir -p "${ext}" "${home_root}" "${wt}"
  git -C "${wt}" init -q -b main
  git -C "${wt}" config user.email selftest@example.com
  git -C "${wt}" config user.name selftest
  git -C "${wt}" commit -q --allow-empty -m init

  # 在 fixture worktree 里跑本脚本。env -i 之外显式喂进需要的变量，避免宿主环境残留干扰
  # （父仓 CLAUDE.md 踩过：Bash 工具的环境变量跨调用保留，上一条命令设的值会被静默沿用）。
  run_guard() {
    local cwd="$1" target_dir="$2" candidates="$3"
    (
      cd "${cwd}" || exit 99
      export PATH HOME
      if [[ -n "${target_dir}" ]]; then export CARGO_TARGET_DIR="${target_dir}"; else unset CARGO_TARGET_DIR; fi
      unset CARGO_WORKTREE_TARGET_ROOT
      export CARGO_WORKTREE_TARGET_ROOT_CANDIDATES="${candidates}"
      bash "${SELF}" 2>&1
    )
  }

  local out rc
  # `set -e` 下命令替换赋值失败会直接终止脚本，而自测正要断言「它失败了」——
  # 必须显式接住退出码。
  capture() {
    set +e
    out="$(run_guard "$1" "$2" "$3")"
    rc=$?
    set -e
  }
  expect() {
    local name="$1" want_rc="$2" want_text="$3"
    if [[ "${want_rc}" == "0" && "${rc}" -ne 0 ]]; then
      echo "自检失败 [${name}]：期望通过(0)，实际 rc=${rc}" >&2
      echo "${out}" | sed 's/^/    /' >&2
      failures=$((failures + 1))
      return
    fi
    if [[ "${want_rc}" != "0" && "${rc}" -eq 0 ]]; then
      echo "自检失败 [${name}]：期望被拒(非 0)，实际通过——守卫在这条上已恒真" >&2
      echo "${out}" | sed 's/^/    /' >&2
      failures=$((failures + 1))
      return
    fi
    if [[ -n "${want_text}" && "${out}" != *"${want_text}"* ]]; then
      echo "自检失败 [${name}]：输出里找不到 '${want_text}'" >&2
      echo "${out}" | sed 's/^/    /' >&2
      failures=$((failures + 1))
      return
    fi
    echo "  ok  ${name}"
  }

  echo "== verify-cargo-worktree-target 反例自测 =="

  # C1 CARGO_TARGET_DIR 为空
  capture "${wt}" "" "${ext}:${home_root}"
  expect "C1 CARGO_TARGET_DIR 为空必须红" 1 "CARGO_TARGET_DIR 为空"

  # C2 basename 错配。顺便从失败信息里取出守卫自己算的期望路径。
  mkdir -p "${ext}/wrong-name"
  capture "${wt}" "${ext}/wrong-name" "${ext}:${home_root}"
  expect "C2 basename 错配必须红且打印期望值与实际值" 1 "期望"
  local expected_ext
  expected_ext="$(printf '%s' "${out}" | sed -n 's/.*期望 \([^ ]*\).*/\1/p' | head -1)"
  if [[ -z "${expected_ext}" || "${expected_ext}" != "${ext}/"* ]]; then
    echo "自检失败 [C2]：没能从失败信息里解析出期望路径（拿到 '${expected_ext}'）" >&2
    failures=$((failures + 1))
    expected_ext="${ext}/__unresolved__"
  fi
  local expected_home="${home_root}/$(basename "${expected_ext}")"

  # C3 正确配对
  mkdir -p "${expected_ext}"
  capture "${wt}" "${expected_ext}" "${ext}:${home_root}"
  expect "C3 正确配对必须绿" 0 "[cargo-target-guard] 通过"

  # C4 落在候选根之外
  mkdir -p "${tmp}/outside/$(basename "${expected_ext}")"
  capture "${wt}" "${tmp}/outside/$(basename "${expected_ext}")" "${ext}:${home_root}"
  expect "C4 target 落在候选根之外必须红" 1 "不匹配"

  # C5 首选根不存在、回落根存在 —— 本次新增的分支，补之前这里是硬失败
  mkdir -p "${expected_home}"
  capture "${wt}" "${expected_home}" "${tmp}/roots/absent:${home_root}"
  expect "C5 首选根缺失时回落根必须能通过" 0 "[cargo-target-guard] 通过"

  # C6 候选根一个都不存在
  capture "${wt}" "${expected_home}" "${tmp}/roots/absent-a:${tmp}/roots/absent-b"
  expect "C6 候选根全缺失必须红" 1 "一个都不存在"

  # C7 两个根都在却用了回落根 —— 必须通过但明确警告缓存分叉
  capture "${wt}" "${expected_home}" "${ext}:${home_root}"
  expect "C7 用回落根但首选根也在，必须通过并警告分叉" 0 "缓存分叉"

  # C8 linked worktree 复用主 clone 的 target
  local linked="${tmp}/linked-wt"
  git -C "${wt}" worktree add -q -b selftest-linked "${linked}" >/dev/null 2>&1
  capture "${linked}" "${expected_ext}" "${ext}:${home_root}"
  expect "C8 linked worktree 复用主 clone target 必须红" 1 "复用了主 clone"

  if [[ "${failures}" -gt 0 ]]; then
    echo "verify-cargo-worktree-target 自检 ${failures} 项失败——守卫已坏，不要相信它的判定" >&2
    return 1
  fi
  echo "verify-cargo-worktree-target 自检全部通过（8 条）"
  return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
  exit $?
fi

main "$@"
