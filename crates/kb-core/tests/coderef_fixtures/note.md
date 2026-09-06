# Dispatch notes

The entry point is `app/services/order_dispatch/conversion.rb` and the caller is
`checkout_controller.rb:284`. See `Billing::InvoiceService#call` and
`Order#confirm_payment!`.

Not refs: `cache.t3.small`, `after_commit :destroy`, `article.cache_key`,
`BundleDescription`, `design-notes.md`, `plan.html`.

```ruby
# app/models/order.rb:1044
after_commit { broadcast(:placed, self) }
```

A `[[wikilink]]` must not become a code ref.
