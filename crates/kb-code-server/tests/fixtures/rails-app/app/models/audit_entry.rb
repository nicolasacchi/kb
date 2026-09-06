# frozen_string_literal: true

# No association, no inbound edge from any other file — the
# `model_referenced_only_from_its_own_file` orphan lane's fixture.
class AuditEntry < ApplicationRecord
end
