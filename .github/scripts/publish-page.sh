#!/bin/bash

# Publishes a run's profiles to the Pages branch, alongside the page and the two registries it
# fetches, with the index rebuilt over everything the branch already holds.
#
# Every guest publishes as soon as it has measured its own corpus, so several runs push to the branch
# at once and the one that lands second is rejected rather than merged. The tree is therefore fetched,
# overlaid and indexed inside the attempt, and the whole of it is retried against whatever landed
# first, which leaves no run waiting on another and drops none of what either published.
#
# Usage: PAGES_BRANCH=<branch> publish-page.sh <remote> <profiles this run produced> <commit message>

set -e -o pipefail

remote=$1
produced=$2
message=$3

# Exactly one publisher wins a round, so the bound has to clear the number of guests pushing to the
# branch, which is every guest of every zkVM the registry lists rather than of the one being profiled.
attempts=16

if [[ -z "${remote}" || -z "${produced}" || -z "${message}" || -z "${PAGES_BRANCH}" ]]; then
  echo "usage: PAGES_BRANCH=<branch> publish-page.sh <remote> <profiles this run produced> <commit message>" >&2
  exit 1
fi

for attempt in $(seq "${attempts}"); do
  # Randomised, since publishers rejected together would otherwise wake together and collide again.
  [[ "${attempt}" -eq 1 ]] || sleep $((RANDOM % 20 + 5))
  rm -rf page

  # A branch that is absent is one no run has published yet, while a branch that is there and will
  # not clone is a fault to retry rather than a reason to publish this run alone over everything
  # already published.
  status=0
  git ls-remote --exit-code --heads "${remote}" "${PAGES_BRANCH}" > /dev/null || status=$?
  case "${status}" in
    0) git clone --quiet --depth 1 --branch "${PAGES_BRANCH}" "${remote}" page || continue ;;
    2)
      git init --quiet --initial-branch "${PAGES_BRANCH}" page
      git -C page remote add origin "${remote}"
      ;;
    *) continue ;;
  esac

  mkdir -p page/profiles
  # A run that measured nothing new still refreshes the page, and has only the profiles the branch
  # already holds to rebuild the index over.
  if [[ -d "${produced}" ]]; then cp -r "${produced}"/. page/profiles/; fi
  cp -r site/. page/
  cp elf-registry.json suite-registry.json page/
  # 404.html is the copy GitHub Pages serves for a path no file sits at, which is what routes a deep
  # link into the page.
  cp page/index.html page/404.html
  ./target/release/zkevm-prof index --dir page/profiles

  git -C page add -A
  if git -C page diff --quiet --cached; then
    echo "the branch already holds what this run would publish"
    exit 0
  fi
  git -C page \
    -c user.name="github-actions[bot]" \
    -c user.email="github-actions[bot]@users.noreply.github.com" \
    commit --quiet -m "${message}"
  if git -C page push --quiet origin "HEAD:${PAGES_BRANCH}"; then
    exit 0
  fi
done

echo "failed to publish to ${PAGES_BRANCH} in ${attempts} attempts" >&2
exit 1
