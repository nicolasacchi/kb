# frozen_string_literal: true

class OrderMailer < ApplicationMailer
  def receipt
    mail(to: "someone@example.com")
  end
end
