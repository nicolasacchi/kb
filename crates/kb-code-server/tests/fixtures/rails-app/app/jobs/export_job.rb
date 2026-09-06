# frozen_string_literal: true

class ExportJob < ApplicationJob
  def perform(id)
    Order.find(id)
  end
end
