import { useUsers } from "../../hooks/useUsers";

// v0.34 W — Settings → Users. Read-only list from GET /api/users
// (configured ∪ observed attribution usernames). Identity is
// attribution, not access control — everyone sees everything.
export default function Users() {
  const { data, error, isLoading } = useUsers();
  const users = data?.users ?? [];

  return (
    <div className="dash">
      <section className="settings__section" aria-label="users">
        <h2 className="settings__h2">Users</h2>
        <p className="settings__hint">
          Identity comes from the edge (Authelia) or per-user tokens;
          everyone sees everything — identity is attribution, not access
          control.
        </p>

        {error != null && (
          <div className="settings__error">
            users unreachable:{" "}
            {error instanceof Error ? error.message : String(error)}
          </div>
        )}

        {isLoading && users.length === 0 && (
          <div className="settings__hint">loading…</div>
        )}

        {!isLoading && users.length === 0 && error == null && (
          <div className="settings__hint">
            no users yet — open an artifact or leave a comment as a named
            identity to populate the observed set
          </div>
        )}

        {users.length > 0 && (
          <table className="dash__table" data-kb-users>
            <thead>
              <tr>
                <th>name</th>
                <th>display</th>
                <th>status</th>
              </tr>
            </thead>
            <tbody>
              {users.map((u) => (
                <tr key={u.name} className="errors__row" data-kb-user-row={u.name}>
                  <td>
                    <span className="kb-user-chip" aria-label={`user ${u.name}`}>
                      {u.name}
                    </span>
                  </td>
                  <td className="settings__hint" style={{ margin: 0 }}>
                    {u.display ?? "—"}
                  </td>
                  <td>
                    <span className="settings__badges">
                      {u.configured && (
                        <span
                          className="settings__badge"
                          title="listed in [identity.users] or operator"
                        >
                          configured
                        </span>
                      )}
                      {u.observed && (
                        <span
                          className="settings__badge"
                          title="seen on history / comments"
                        >
                          observed
                        </span>
                      )}
                      {!u.configured && !u.observed && (
                        <span className="settings__badge">—</span>
                      )}
                    </span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </div>
  );
}
