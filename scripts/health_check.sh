#!/bin/bash
# HealthCheck — run on VPS to verify everything is working.
# Usage: bash /opt/cow-solver/scripts/health_check.sh
#
# Returns plain text report. Non-zero exit code if anything is broken.
# Can be run by a human, a cron job, or an automated agent on the VPS.

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'
FAIL=0

echo "═══════════════════════════════════════════════════════════════"
echo " CoW Solver Health Check - $(date)"
echo "═══════════════════════════════════════════════════════════════"
echo ""

# ── 1. Are all containers running? ──────────────────────────────
echo "── CONTAINERS ──"
for name in cow-solver; do
  status=$(docker inspect -f '{{.State.Status}}' "$name" 2>/dev/null)
  if [ "$status" = "running" ]; then
    echo -e "  ${GREEN}OK${NC}  $name"
  else
    echo -e "  ${RED}DOWN${NC}  $name (status: ${status:-not found})"
    FAIL=1
  fi
done
echo ""

# ── 2. CoW Solver: producing solutions? ────────────────────────
echo "── COW SOLVER ──"
COW_DB="/var/lib/docker/volumes/cow-solver_solver-data/_data/replay.db"
if [ -f "$COW_DB" ]; then
  DB_SIZE=$(du -h "$COW_DB" | cut -f1)
  echo "  DB size: $DB_SIZE"

  # Check last 5 auctions
  LAST5=$(sqlite3 "$COW_DB" "SELECT result, our_score_wei, response_time_ms FROM auction_log ORDER BY received_at DESC LIMIT 5;" 2>/dev/null)
  if [ -z "$LAST5" ]; then
    echo -e "  ${RED}FAIL${NC}  No auctions in DB"
    FAIL=1
  else
    EMPTY_COUNT=$(echo "$LAST5" | grep -c "^empty|")
    SUBMITTED_COUNT=$(echo "$LAST5" | grep -c "^submitted|")
    TIMEOUT_COUNT=$(echo "$LAST5" | grep -c "timeout")

    if [ "$EMPTY_COUNT" -ge 5 ]; then
      echo -e "  ${RED}FAIL${NC}  Last 5 auctions ALL empty (likely timeout bug)"
      FAIL=1
    elif [ "$SUBMITTED_COUNT" -ge 1 ]; then
      echo -e "  ${GREEN}OK${NC}  Submitting solutions ($SUBMITTED_COUNT/5 recent)"
    else
      echo -e "  ${YELLOW}WARN${NC}  Mixed results — check logs"
    fi

    # Check scores
    SCORE=$(sqlite3 "$COW_DB" "SELECT our_score_wei FROM auction_log WHERE CAST(our_score_wei AS INTEGER) > 0 ORDER BY received_at DESC LIMIT 1;" 2>/dev/null)
    if [ -n "$SCORE" ] && [ "$SCORE" != "0" ]; then
      SCORE_GWEI=$((SCORE / 1000000000))
      echo -e "  ${GREEN}OK${NC}  Latest score: ${SCORE_GWEI} gwei"
    else
      echo -e "  ${RED}FAIL${NC}  No non-zero scores found"
      FAIL=1
    fi

    # Check for winner data (competition tracker)
    WINNERS=$(sqlite3 "$COW_DB" "SELECT COUNT(*) FROM auction_log WHERE winning_score_wei IS NOT NULL AND winning_score_wei != '0' AND winning_score_wei != '';" 2>/dev/null)
    echo "  Winner data: $WINNERS auctions"

    # Total auctions
    TOTAL=$(sqlite3 "$COW_DB" "SELECT COUNT(*) FROM auction_log;" 2>/dev/null)
    SUBMITTED_TOTAL=$(sqlite3 "$COW_DB" "SELECT COUNT(*) FROM auction_log WHERE result='submitted';" 2>/dev/null)
    echo "  Total: $TOTAL auctions ($SUBMITTED_TOTAL submitted)"
  fi
else
  echo -e "  ${RED}FAIL${NC}  Replay DB not found at $COW_DB"
  FAIL=1
fi

# Check last log line for errors
COW_LAST=$(docker logs cow-solver --tail 3 2>&1 | grep -c "error\|panic\|FATAL")
if [ "$COW_LAST" -gt 0 ]; then
  echo -e "  ${YELLOW}WARN${NC}  Recent errors in logs"
fi
echo ""

# ── 3. Disk space ──────────────────────────────────────────────
echo "── DISK ──"
DISK_USED=$(df -h / | tail -1 | awk '{print $5}')
DISK_AVAIL=$(df -h / | tail -1 | awk '{print $4}')
DISK_PCT=$(df / | tail -1 | awk '{print $5}' | tr -d '%')
if [ "$DISK_PCT" -gt 85 ]; then
  echo -e "  ${RED}FAIL${NC}  Disk ${DISK_USED} used, ${DISK_AVAIL} free — DANGER"
  FAIL=1
elif [ "$DISK_PCT" -gt 70 ]; then
  echo -e "  ${YELLOW}WARN${NC}  Disk ${DISK_USED} used, ${DISK_AVAIL} free"
else
  echo -e "  ${GREEN}OK${NC}  Disk ${DISK_USED} used, ${DISK_AVAIL} free"
fi

# ── 4. Memory ──────────────────────────────────────────────────
echo ""
echo "── MEMORY ──"
MEM_USED=$(free -h | grep Mem | awk '{print $3}')
MEM_TOTAL=$(free -h | grep Mem | awk '{print $2}')
MEM_AVAIL=$(free -h | grep Mem | awk '{print $7}')
echo -e "  ${GREEN}OK${NC}  ${MEM_USED} / ${MEM_TOTAL} (${MEM_AVAIL} available)"

# ── 5. Data accessibility test ─────────────────────────────────
echo ""
echo "── DATA ACCESS ──"
# Can we read the CoW DB without docker exec?
if sqlite3 "$COW_DB" "SELECT 1;" >/dev/null 2>&1; then
  echo -e "  ${GREEN}OK${NC}  CoW replay DB readable directly (no docker exec needed)"
else
  echo -e "  ${RED}FAIL${NC}  CoW replay DB not readable — WAL lock or missing sqlite3"
  FAIL=1
fi

# SCP path hint
echo "  Pull command:"
echo "    scp root@VPS:${COW_DB} replay.db"

echo ""
echo "═══════════════════════════════════════════════════════════════"
if [ "$FAIL" -eq 0 ]; then
  echo -e " ${GREEN}ALL SYSTEMS GO${NC}"
else
  echo -e " ${RED}ISSUES DETECTED — SEE ABOVE${NC}"
fi
echo "═══════════════════════════════════════════════════════════════"
exit $FAIL
