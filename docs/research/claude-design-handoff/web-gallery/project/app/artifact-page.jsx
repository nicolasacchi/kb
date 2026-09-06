// Synthetic artifact content rendered inside the iframe
function ArtifactPage({ a }) {
  const accent = window.TAG_COLORS[a.accentTag] || '#888';
  return (
    <div className="art-page">
      <div className="art-eyebrow" style={{ color: accent }}>{a.tags.join(' · ')}</div>
      <h1 className="art-h1">{a.title}</h1>
      <p className="art-lede">{a.summary}</p>
      <div className="art-meta-row">
        <span>{a.words.toLocaleString()} words</span>
        <span>·</span>
        <span>{Math.round(a.words / 240)} min read</span>
        <span>·</span>
        <span>updated {a.age} ago</span>
      </div>
      <div className="art-rule"/>

      <h2>The shape of the problem</h2>
      <p>Every approach to {a.tags[0]} eventually runs into the same wall: composability across boundaries that the type system can&rsquo;t see. The good news is that we have a small set of patterns that handle the 80% case cleanly. The bad news is the 20% case is what your production system will hit at 3am.</p>

      <p>Let&rsquo;s walk through the canonical setup, then look at where it falls down.</p>

      <h3>A first pass</h3>
      <pre className="art-code"><code>{`async fn fetch_user(id: UserId) -> Result<User, Error> {
    let conn = pool.get().await?;
    let row = conn.query_one("SELECT * FROM users WHERE id = $1", &[&id]).await?;
    Ok(User::from_row(row))
}`}</code></pre>

      <p>This is the form everyone reaches for, and it&rsquo;s correct. The <code>?</code> operator hides the actual shape of the error flow, which is fine until it isn&rsquo;t.</p>

      <h3>What goes wrong</h3>
      <ul>
        <li>Errors lose context as they bubble. By the time they reach the top, you&rsquo;ve lost the <em>which</em> of which user.</li>
        <li>Concurrent calls are easy to start, hard to coordinate. Cancellation discipline takes practice.</li>
        <li>The boundary between &ldquo;recoverable&rdquo; and &ldquo;abort the request&rdquo; lives in your head, not the types.</li>
      </ul>

      <blockquote>
        <p>The compiler will save you from a category of bug. It will not save you from a confused mental model.</p>
      </blockquote>

      <h3>A second pass with structure</h3>
      <pre className="art-code"><code>{`#[derive(thiserror::Error, Debug)]
enum FetchError {
    #[error("user {0} not found")]
    NotFound(UserId),
    #[error("database unavailable")]
    Db(#[from] sqlx::Error),
}`}</code></pre>

      <p>Now the call site can <code>match</code> on something meaningful. The <code>#[from]</code> propagation keeps the <code>?</code> ergonomics. You pay one enum variant per failure mode you actually care about, which is the right price.</p>

      <h2>Patterns worth memorizing</h2>
      <ol>
        <li><strong>Error-as-data, not error-as-string.</strong> Strings are for humans. Match arms are for code.</li>
        <li><strong>Boundary types.</strong> What leaves your crate is not the same as what flows internally.</li>
        <li><strong>Context, not wrapping.</strong> Use <code>.context()</code> at the seams, not at every layer.</li>
      </ol>

      <h2>Tradeoffs</h2>
      <table className="art-table">
        <thead><tr><th>Approach</th><th>Allocations</th><th>Match-friendly</th><th>Boilerplate</th></tr></thead>
        <tbody>
          <tr><td>anyhow</td><td>1 box</td><td>no</td><td>none</td></tr>
          <tr><td>thiserror</td><td>0</td><td>yes</td><td>per variant</td></tr>
          <tr><td>hand-written</td><td>0</td><td>yes</td><td>a lot</td></tr>
        </tbody>
      </table>

      <h2>What to take away</h2>
      <p>Pick anyhow at the top of your binary. Pick thiserror at the top of your library. Don&rsquo;t mix them in the same module. The rest is taste.</p>

      <p className="art-foot">Last edited {a.age} ago · {a.backlinks} backlinks · {a.outlinks} outgoing</p>
    </div>
  );
}

window.ArtifactPage = ArtifactPage;
