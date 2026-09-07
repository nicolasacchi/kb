# frozen_string_literal: true

class OrdersController < ApplicationController
  def index
    @orders = Order.all
  end

  def show
    ExportJob.perform_later(params[:id])
    OrderMailer.receipt.deliver_later
  end

  def cancel
    redirect_to orders_path
  end

  # V72-I2 — no explicit render, so the lens resolves this action's template
  # by convention. That template is HAML (`summary.html.haml`), which is the
  # whole point: `find_view_files` matches on the STEM, so a `.haml` view is
  # reachable exactly like a `.erb` one — and the end-to-end goldens now
  # prove it through the real ingest pipeline, not just a unit call.
  def summary
    @orders = Order.all
  end

  # Public, and no route reaches it — the `action_without_route` orphan
  # lane's fixture.
  def export
    redirect_to orders_path
  end

  private

  def audit_trail
    Rails.logger.info(t("orders.index.title"))
  end
end
