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
