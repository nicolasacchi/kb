# frozen_string_literal: true

# Never enqueued anywhere — the `job_never_enqueued` orphan lane's fixture.
class CleanupJob < ApplicationJob
  def perform
    LineItem.delete_all
  end
end
