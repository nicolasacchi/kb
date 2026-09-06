# frozen_string_literal: true

class BackofficeController < ApplicationController
  include Loggable

  def welcome
    render(RowComponent.new(item: @item))
  end

  def ghost
    render(GhostComponent.new)
  end

  def translations
    t('controllers.backoffice.welcome.title')
    t('shared.hello')
    t(dynamic_key)
    head :ok
  end

  def enqueue
    NotifyJob.perform_later
    job_var.perform_later
    head :ok
  end

  def deliver
    BackofficeMailer.welcome.deliver_later
    mailer_var.welcome.deliver_later
    head :ok
  end
end
