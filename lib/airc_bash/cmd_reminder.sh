# Sourced by airc. cmd_reminder — idle-message-nudge cadence control.
#
# Function exported back to airc's dispatch:
#   cmd_reminder  — show / set / pause / disable the auto-nudge interval
#                   that the monitor loop emits when the room has been
#                   silent for N seconds. `airc reminder 300` sets it to
#                   5 min, `off`/`pause` disable, no-arg shows current.
#
# External cross-references (call-time): die, ensure_init, get_config_val,
# set_config_val, AIRC_REMINDER (env override).
#
# Extracted from airc as part of #152 Phase 3 file split — the final
# structural sweep that takes the bash top-level back below ~1500 lines.

cmd_reminder() {
  ensure_init
  local arg="${1:-status}"
  local reminder_file="$AIRC_WRITE_DIR/reminder"

  case "$arg" in
    off|0)
      rm -f "$reminder_file"
      echo "  Reminders off."
      ;;
    pause)
      echo "0" > "$reminder_file"
      echo "  Reminders paused. 'airc reminder <seconds>' to resume."
      ;;
    status)
      if [ -f "$reminder_file" ]; then
        local val; val=$(cat "$reminder_file")
        if [ "$val" = "0" ]; then
          echo "  Reminders paused."
        else
          echo "  Reminder every ${val}s."
        fi
      else
        echo "  Reminders off."
      fi
      ;;
    *)
      echo "$arg" > "$reminder_file"
      echo "  Reminder every ${arg}s if no messages."
      ;;
  esac
}
