import {
  LOCK_ACTIONS,
  LOCK_REASON_MAX,
  UNLOCK_FORBIDDEN_FIELDS,
} from "./contracts.js";

export function validateUpdateArgs(args: Record<string, unknown>): void {
  const lockAction = args.lock_action;
  const lockReason = args.lock_reason;

  if (
    lockAction !== undefined &&
    (typeof lockAction !== "string" ||
      !LOCK_ACTIONS.includes(lockAction as (typeof LOCK_ACTIONS)[number]))
  ) {
    throw new Error(
      `Invalid lock_action: ${lockAction}. Must be one of: ${LOCK_ACTIONS.join(", ")}`,
    );
  }

  if (lockReason !== undefined && lockAction !== "lock") {
    throw new Error("lock_reason may only be provided when lock_action is 'lock'");
  }
  if (lockReason !== undefined && typeof lockReason !== "string") {
    throw new Error("lock_reason must be a string");
  }
  if (typeof lockReason === "string" && [...lockReason].length > LOCK_REASON_MAX) {
    throw new Error(
      `lock_reason must be at most ${LOCK_REASON_MAX} characters`,
    );
  }

  if (lockAction === "unlock") {
    for (const field of UNLOCK_FORBIDDEN_FIELDS) {
      const provided =
        field === "clear_expiry" ? args[field] === true : args[field] !== undefined;
      if (provided) {
        throw new Error(
          `Field '${field}' cannot be combined with lock_action='unlock'. ` +
            `Unlock is a lifecycle-only operation.`,
        );
      }
    }
  }
}
